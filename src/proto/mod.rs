#[cfg(feature = "wireguard")]
pub(crate) mod wireguard;
#[cfg(feature = "xray")]
pub mod xray;

#[cfg(feature = "amnezia-wg")]
pub(crate) mod amnezia_wg;
