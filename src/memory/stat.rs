use std::fmt;

#[derive(Debug, Clone)]
pub enum StatType {
    Conn(Kind),
    Inbound(Kind),
    Outbound(Kind),
}

impl fmt::Display for StatType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StatType::Conn(Kind::Uplink) => write!(f, "uplink"),
            StatType::Conn(Kind::Downlink) => write!(f, "downlink"),
            StatType::Conn(Kind::Online) => write!(f, "online"),
            StatType::Inbound(Kind::Uplink) => write!(f, "uplink"),
            StatType::Inbound(Kind::Downlink) => write!(f, "downlink"),
            StatType::Inbound(Kind::Online) => write!(f, "Not implemented"),
            StatType::Outbound(Kind::Uplink) => write!(f, "uplink"),
            StatType::Outbound(Kind::Downlink) => write!(f, "downlink"),
            StatType::Outbound(Kind::Online) => write!(f, "Not implemented"),

            StatType::Conn(Kind::Unknown)
            | StatType::Inbound(Kind::Unknown)
            | StatType::Outbound(Kind::Unknown) => write!(f, "unknown"),
        }
    }
}

#[derive(Debug, Clone)]
pub enum Kind {
    Uplink,
    Downlink,
    Online,
    Unknown,
}

impl Kind {
    pub fn from_path(path: &str) -> Kind {
        if let Some(last) = path.split('.').next_back() {
            match last {
                "uplink" => Kind::Uplink,
                "downlink" => Kind::Downlink,
                "online" => Kind::Online,
                _ => Kind::Unknown,
            }
        } else {
            Kind::Unknown
        }
    }
}

impl fmt::Display for Kind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Kind::Uplink => write!(f, "uplink"),
            Kind::Downlink => write!(f, "downlink"),
            Kind::Online => write!(f, "online"),
            Kind::Unknown => write!(f, "unknown"),
        }
    }
}
