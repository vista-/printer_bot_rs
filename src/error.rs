use thiserror::Error;

#[derive(Error, Debug)]
pub enum PrinterBotError {
    #[error("io error")]
    Io(#[from] std::io::Error),
    #[error("teloxide error")]
    Teloxide(#[from] teloxide_core::RequestError),
    #[error("file download error")]
    Download(#[from] teloxide_core::DownloadError),
    #[error("image error")]
    Image(#[from] image::ImageError),
    #[error("invalid image")]
    InvalidImage,
    #[error("thread panic")]
    ThreadPanic,
    #[error("qrcode generator fail")]
    QrCodeGen(#[from] qrcode_generator::QRCodeError),
    #[error("image not found")]
    ImageNotFound,
}
