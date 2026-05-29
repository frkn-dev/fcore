#[cfg(feature = "wireguard")]
use defguard_wireguard_rs::{error::WireguardInterfaceError, net::IpAddrParseError};
use thiserror::Error as ThisError;

#[derive(Debug, ThisError)]
pub enum Error {
    #[error(transparent)]
    Database(#[from] tokio_postgres::Error),

    #[error("DB conflict")]
    Conflict,

    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error(transparent)]
    UrlParse(#[from] url::ParseError),

    #[error(transparent)]
    Http(#[from] reqwest::Error),

    #[error(transparent)]
    Json(#[from] serde_json::Error),

    #[error(transparent)]
    Yaml(#[from] serde_yaml::Error),

    #[error(transparent)]
    Join(#[from] tokio::task::JoinError),

    #[cfg(feature = "xray")]
    #[error(transparent)]
    XrayUri(#[from] tonic::codegen::http::uri::InvalidUri),

    #[cfg(feature = "xray")]
    #[error(transparent)]
    XrayTransport(#[from] tonic::transport::Error),

    #[cfg(feature = "xray")]
    #[error(transparent)]
    Grpc(#[from] Box<tonic::Status>),

    #[error(transparent)]
    SerdeUrlEnc(#[from] serde_urlencoded::ser::Error),

    #[error(transparent)]
    Zmq(#[from] zmq::Error),

    #[cfg(feature = "wireguard")]
    #[error(transparent)]
    Wireguard(#[from] WireguardInterfaceError),

    #[cfg(feature = "wireguard")]
    #[error(transparent)]
    IpParseError(#[from] IpAddrParseError),

    #[error(transparent)]
    TomlDeError(#[from] toml::de::Error),

    #[error("Error: {0}")]
    Custom(String),

    #[error("SerializationError: {0}")]
    SerializationError(String),

    #[error(transparent)]
    RkyvSerialize(
        #[from]
        rkyv::ser::serializers::CompositeSerializerError<
            std::convert::Infallible,
            rkyv::ser::serializers::AllocScratchError,
            rkyv::ser::serializers::SharedSerializeMapError,
        >,
    ),

    #[error(transparent)]
    ConvertInfallibleError(#[from] std::convert::Infallible),
}

#[derive(Debug, ThisError)]
pub enum SyncError {
    #[error("Database operation failed: {0}")]
    Database(#[from] Error),

    #[error("Memory operation failed: {0}")]
    Memory(String),

    #[error("Validation failed: {0}")]
    Validation(String),

    #[error(transparent)]
    Zmq(#[from] zmq::Error),

    #[error(transparent)]
    RkyvSerialize(
        #[from]
        rkyv::ser::serializers::CompositeSerializerError<
            std::convert::Infallible,
            rkyv::ser::serializers::AllocScratchError,
            rkyv::ser::serializers::SharedSerializeMapError,
        >,
    ),

    #[error("Inconsistent state between database and memory for {resource} {id}")]
    InconsistentState { resource: String, id: uuid::Uuid },

    #[error("Resource not found: {resource} {id}")]
    ResourceNotFound { resource: String, id: uuid::Uuid },

    #[error("Concurrent modification detected for {resource} {id}")]
    ConcurrentModification { resource: String, id: uuid::Uuid },
}

pub type Result<T> = std::result::Result<T, Error>;

impl From<String> for Error {
    fn from(err: String) -> Self {
        Error::Custom(err)
    }
}

impl<T: std::fmt::Debug> From<tokio::sync::mpsc::error::SendError<T>> for Error {
    fn from(err: tokio::sync::mpsc::error::SendError<T>) -> Self {
        Error::Custom(format!("SendError: {:?}", err))
    }
}

#[cfg(feature = "xray")]
impl From<tonic::Status> for Error {
    fn from(status: tonic::Status) -> Self {
        Error::Grpc(Box::new(status))
    }
}

impl warp::reject::Reject for Error {}
