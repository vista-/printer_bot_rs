#![feature(test)]

use std::env;

use error::PrinterBotError;
use image::DynamicImage;
use log::*;
use teloxide_core::net::Download;
use teloxide_core::types::{InputFile, InputMedia, InputMediaPhoto, ChatId, FileId};
use teloxide_core::{
    payloads::GetUpdatesSetters,
    requests::{Requester, RequesterExt},
};

use std::sync::Arc;
use std::collections::HashSet;

use tokio::sync::Mutex;

use qrcode_generator::{QrCodeEcc, QRCodeError};

use crate::driver::{PrinterCommand, PrinterCommandMode, PrinterExpandedMode, PrinterMode};
use crate::ratelimit::{MultiRateLimiter};

mod driver;
mod error;
mod ratelimit;

struct Settings {
    dpi_600: bool,
}

#[tokio::main]
async fn main() -> Result<(), PrinterBotError> {
    dotenvy::dotenv().ok();
    env_logger::init();

    let token = env::var("BOT_TOKEN").expect("BOT_TOKEN is not set");
    let password = env::var("PASSWORD").expect("PASSWORD is not set");

    let owner_id: ChatId = ChatId(
        env::var("OWNER_ID")
            .expect("OWNER_ID is not set")
            .parse()
            .expect("invalid OWNER_ID"),
    );

    let bot = teloxide_core::Bot::new(token).parse_mode(teloxide_core::types::ParseMode::Html);

    bot.send_message(owner_id, "Bot is ready to print :3").await?;

    info!("Started polling");

    let mut offset: u32 = 0;

    let settings = Arc::new(Settings { dpi_600: true });

    let limiter = MultiRateLimiter::new();
    let authenticated_users: Arc<Mutex<HashSet<ChatId>>> = Arc::new(Mutex::new(HashSet::new()));

    type SharedPrinter = Arc<Mutex<()>>;
    let printer_mutex: SharedPrinter = Arc::new(Mutex::new(()));

    loop {
        let updates = bot.get_updates().offset(offset as i32).await;

        match updates {
            Ok(updates) => {
                for update in updates {
                    offset = update.id.0 + 1;

                    if let teloxide_core::types::UpdateKind::Message(message) = update.kind {
                        trace!("{:?} {:?} ({:?}) | incoming message", message.chat.first_name(), message.chat.last_name(), message.chat.id);
                        if message.chat.id != owner_id {
                            trace!("{:?} {:?} ({:?}) | is not owner", message.chat.first_name(), message.chat.last_name(), message.chat.id);
                            let mut authed = authenticated_users.lock().await;
                            if !authed.contains(&message.chat.id) {
                                trace!("{:?} {:?} ({:?}) | is not authenticated yet", message.chat.first_name(), message.chat.last_name(), message.chat.id);
                                if let Some(text) = message.text() {
                                    debug!("{:?} {:?} ({:?}) | said: {:?}", message.chat.first_name(), message.chat.last_name(), message.chat.id, text);
                                    if text.trim().to_uppercase() == password {
                                        authed.insert(message.chat.id);
                                        debug!("{:?} {:?} ({:?}) | authenticated", message.chat.first_name(), message.chat.last_name(), message.chat.id);
                                        bot.send_message(message.chat.id, "Access granted, have fun :3").await.ok();
                                    } else {
                                        bot.send_message(message.chat.id, "Welcome to Cooper and Stally's Sticker Printer bot, space creature!\nPlease think of other creatures and only print as much as you need.\nEnter the current event's abbreviation (e.g. 38C3) to gain access!\n\nInfo, issues, out of paper? Contact @samadaul or @stally0").await.ok();
                                        continue;
                                    }
                                } else {
                                    bot.send_message(message.chat.id, "Welcome to Cooper and Stally's Sticker Printer bot!\nPlease think of others and only print as much as you need.\nEnter the current event's abbreviation (e.g. 38C3) to gain access!\n\nInfo, issues, out of paper? Contact @samadaul or @stally0").await.ok();
                                    continue;
                                }
                            }
                            drop(authed);

                        }

                        if let Some((file_id, file_ext)) =
                            extract_photo_from_message(&bot, &message).await?
                        {
                            debug!("{:?} {:?} ({:?}) | sent an image to print", message.chat.first_name(), message.chat.last_name(), message.chat.id);
                            trace!("{:?} {:?} ({:?}) | rate limit stats: {:?}", message.chat.first_name(), message.chat.last_name(), message.chat.id, limiter.get_usage(&message.chat.id.to_string()));

                            match limiter.check_rate_limit(&message.chat.id.to_string()).await {
                                Ok(()) => {
                                    trace!("{:?} {:?} ({:?}) | rate limit passed", 
                                        message.chat.first_name(), message.chat.last_name(), message.chat.id);
                                    // Process message
                                }
                                Err(limit_type) => {
                                    debug!("{:?} {:?} ({:?}) | rate limited: {}", 
                                        message.chat.first_name(), message.chat.last_name(), message.chat.id, limit_type);
                                    bot.send_message(message.chat.id, 
                                        "Hold your horses! You're printing way too fast, spaceman! The rate limits are: 5 stickers per 5 minutes, 10 per hour, 20 per day.")
                                        .await.ok();
                                    continue;
                                }
                            }

                            let file_path = download_file(&bot, &file_id, &file_ext).await?;

                            let lines = match render_image(&file_path, &settings) {
                                Ok(lines) => lines,
                                Err(PrinterBotError::InvalidImage) => {
                                    bot.send_message(
                                        message.chat.id, 
                                        "Sorry! Your image is too tall/narrow to print nicely. Try cropping or rotating your image!"
                                    ).await.ok();
                                    continue;
                                }
                                Err(_) => continue, // Propagate other errors
                            };

                            bot.send_message(message.chat.id, "Your sticker is printing now!").await.ok();
                            if let Err(err) = queue_print_lines(lines, &settings, printer_mutex.clone()).await {
                                error!("print failed, {:?}", err);
                                bot.send_message(owner_id, format!("print failed, reason: {:?}", err)).await.ok();
                                continue;
                            }

                            bot.send_message(message.chat.id, "Print done! Enjoy! :3").await.ok();
                        }
                    }
                }
            }
            Err(err) => {
                error!("{:?}", err);
                bot.send_message(owner_id, format!("RequestError: {:#?}", err)).await.ok();
                continue;
            }
        }
    }
}

async fn queue_print_lines(lines: Vec<[u8; 90]>, settings: &Arc<Settings>, printer_mutex: Arc<Mutex<()>>) -> Result<(), PrinterBotError> {
    let _guard = printer_mutex.lock().await; // lock acquired
    // Run blocking code in a separate thread pool

    let result = tokio::task::spawn_blocking({
        let settings = settings.clone();
        move || print_lines(lines, &settings)
    }).await;
    match result {
        Ok(print_result) => print_result, // propagate your original Result
        Err(join_err) => {
            // Map the JoinError into your PrinterBotError
            eprintln!("Printer task panicked: {:?}", join_err);
            Err(PrinterBotError::ThreadPanic) // create a ThreadPanic variant
        }
    }
}

async fn extract_photo_from_message(
    bot: &teloxide_core::adaptors::DefaultParseMode<teloxide_core::Bot>,
    message: &teloxide_core::types::Message,
) -> Result<Option<(String, String)>, PrinterBotError> {
    if let Some(photo) = message.photo() {
        let biggest = photo.iter().max_by_key(|x| x.width);

        if let Some(biggest) = biggest {
            return Ok(Some((biggest.file.id.to_string(), "jpg".to_string())));
        }
    }

    if let Some(sticker) = message.sticker() {
        if sticker.is_static() {
            return Ok(Some((sticker.file.id.to_string(), "webp".to_string())));
        } else {
            bot.send_message(message.chat.id, "Can't print animated stickers")
                .await?;
        }
    }

    if let Some(document) = message.document() {
        if let Some(mime_type) = &document.mime_type {
            let extension = match mime_type.as_ref() {
                "image/jpeg" => "jpg",
                "image/png" => "png",
                "image/gif" => "gif",
                "image/webp" => "webp",
                "image/tiff" => "tiff",
                "image/bmp" => "bmp",
                _ => {
                    bot.send_message(message.chat.id, "Can't print documents that are not images")
                        .await?;
                    return Ok(None);
                }
            };

            return Ok(Some((document.file.id.to_string(), extension.to_string())));
        }

        return Ok(None);
    }

    if let Some(text) = message.text() {
        // Check if it's a QR code command
        if text.starts_with("/qr ") {
            let qr_content = text.strip_prefix("/qr ").unwrap_or("");
            
            if qr_content.is_empty() {
                bot.send_message(message.chat.id, "Usage: /qr <content>\nExample: /qr https://example.com")
                    .await?;
                return Ok(None);
            }

            // Generate QR code
            match generate_qr_code(qr_content) {
                Ok(qr_png_data) => {
                    // Send QR code as photo to get a file_id back
                    let input_file = InputFile::memory(qr_png_data).file_name("qrcode.png");
                    
                    match bot.send_photo(message.chat.id, input_file).await {
                        Ok(sent_message) => {
                            if let Some(photo) = sent_message.photo() {
                                let biggest = photo.iter().max_by_key(|x| x.width);
                                if let Some(biggest) = biggest {
                                    return Ok(Some((biggest.file.id.to_string(), "jpg".to_string())));
                                }
                            }
                            warn!("QR code generated but couldn't get file ID");
                            return Ok(None);
                        }
                        Err(e) => {
                            error!("Failed to send QR code");
                            return Err(PrinterBotError::Teloxide(e));
                        }
                    }
                }
                Err(e) => {
                    warn!("Failed to generate QR code");
                    return Err(PrinterBotError::QrCodeGen(e));
                }
            }
        }
    }
    Ok(None)
}

fn generate_qr_code(content: &str) -> Result<Vec<u8>, qrcode_generator::QRCodeError> {
    let png_data = qrcode_generator::to_png_to_vec(content, QrCodeEcc::Low, 1024)?;
    debug!("Generated QR code: {} bytes", png_data.len());
    
    // Check PNG signature (should start with [137, 80, 78, 71])
    if png_data.len() >= 4 {
        debug!("PNG signature: {:?}", &png_data[0..4]);
    }
    
    Ok(png_data)
}

async fn download_file(
    bot: &teloxide_core::adaptors::DefaultParseMode<teloxide_core::Bot>,
    file_id: &str,
    file_ext: &str,
) -> Result<String, PrinterBotError> {
    let file = bot.get_file(FileId::from(file_id.to_string())).await?;
    let file_path = format!("/home/sticker/printed-files/{file_id}.{file_ext}");
    let mut dst = tokio::fs::File::create(&file_path).await?;
    bot.download_file(&file.path, &mut dst).await?;
    Ok(file_path)
}

fn render_image(file_path: &str, settings: &Settings) -> Result<Vec<[u8; 90]>, PrinterBotError> {
    use image::ImageReader;

    let img: DynamicImage;
    match ImageReader::open(file_path) {
        Ok(img_handle) => {
            img = match img_handle.decode() {
                Ok(img) => img,
                Err(e) => { return Err(PrinterBotError::Image(e))}
            }
        }
        Err(_) => {
            return Err(PrinterBotError::ImageNotFound);
        }
    };

    // Limit stickers ratio (so people don't print incredibly long stickers)

    let ratio = img.height() as f32 / img.width() as f32;

    if ratio > 1.5 {
        return Err(PrinterBotError::InvalidImage);
    }

    // remove transparency
    let img = img.into_rgba8();

    let background_color = image::Rgba([255, 255, 255, 255]);
    let mut background_image =
        image::ImageBuffer::from_pixel(img.width(), img.height(), background_color);
    image::imageops::overlay(&mut background_image, &img, 0, 0);

    // convert to grayscale

    let img = image::imageops::grayscale(&background_image);

    // resize

    let new_width = 720; //630 per la carta piccola

    let new_height = new_width * img.height() / img.width() * if settings.dpi_600 { 2 } else { 1 };

    let mut img = image::imageops::resize(
        &img,
        new_width,
        new_height,
        image::imageops::FilterType::Lanczos3,
    );

    // gamma correction
    // match the brightness of the previous implementation
    let gamma_correction = 5.14;

    img.pixels_mut()
        .for_each(|x| x.0 = [(255.0 * (x.0[0] as f32 / 255.0).powf(1.0 / gamma_correction)) as u8]);

    use exoquant::*;

    let palette = vec![Color::new(0, 0, 0, 255), Color::new(255, 255, 255, 255)];

    let ditherer = ditherer::FloydSteinberg::vanilla();
    let colorspace = SimpleColorSpace::default();
    let remapper = Remapper::new(&palette, &colorspace, &ditherer);

    let image = img
        .pixels()
        .map(|x| Color::new(x.0[0], x.0[0], x.0[0], 255))
        .collect::<Vec<Color>>();

    let indexed_data = remapper.remap(&image, img.width() as usize);
 
    // convert to vec of line bits

    let mut lines = Vec::new();

    for y in 0..img.height() {
        let mut line = [0u8; 90];

        for x in 0..img.width() {
            let i = y * img.width() + x;
            let i = indexed_data[i as usize];

            let byte = x / 8;
            let bit = x % 8;

            if i == 0 {
                line[89 - byte as usize] |= 1 << bit;
            }
        }

        lines.push(line);
    }

    Ok(lines)
}

fn print_lines(lines: Vec<[u8; 90]>, settings: &Settings) -> Result<(), PrinterBotError> {
    let mut printer = driver::PrinterCommander::main("/dev/usb/lp0")?;

    printer.send_command(PrinterCommand::Reset)?;
    printer.send_command(PrinterCommand::Initialize)?;

    // information
    printer.send_command(PrinterCommand::StatusInfoRequest)?;

    let status = printer.read_status()?;
    trace!("{:#?}", status);

    printer.send_command(PrinterCommand::SetCommandMode(PrinterCommandMode::Raster))?;

    printer.send_command(PrinterCommand::SetPrintInformation(
        status,
        lines.len() as i32,
    ))?;

    printer.send_command(PrinterCommand::SetExpandedMode(PrinterExpandedMode {
        cut_at_end: true,
        high_resolution_printing: settings.dpi_600,
    }))?;

    printer.send_command(PrinterCommand::SetMode(PrinterMode { auto_cut: true }))?;

    // this is needed for the auto cut
    printer.send_command(PrinterCommand::SetPageNumber(1))?;

    printer.send_command(PrinterCommand::SetMarginAmount(0))?;

    debug!("printing {} lines", lines.len());

    for line in lines {
        printer.send_command(PrinterCommand::RasterGraphicsTransfer(line))?;
    }

    printer.send_command(PrinterCommand::PrintWithFeeding)?;

    trace!("{:#?}", printer.read_status()?);
    trace!("{:#?}", printer.read_status()?);
    trace!("{:#?}", printer.read_status()?);

    Ok(())
}

#[allow(dead_code)]
fn debug_print_dithered(data: &[u8], width: u32, height: u32) -> Result<(), PrinterBotError> {
    let img = image::ImageBuffer::from_fn(width, height, |x, y| {
        let i = y * width + x;
        let i = data[i as usize];
        image::Rgba([i * 255, i * 255, i * 255, 255])
    });
    img.save("/tmp/out_dithered.png")?;

    Ok(())
}
