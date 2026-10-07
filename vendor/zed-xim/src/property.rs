use alloc::format;
use alloc::string::String;

/// Number of distinct property names used to pass large XIM requests.
///
/// Xlib cycles through `_clientN` for N in 0..=20. IBus' IMdkit grows an
/// offset cache by one entry per distinct property atom and corrupts its heap
/// past 22 atoms, so the set must stay bounded.
const DATA_PROPERTY_COUNT: u16 = 21;

/// Name of the property that carries the request with this sequence number.
pub fn data_property_name(sequence: u16) -> String {
    format!("_XIM_DATA_{}", sequence % DATA_PROPERTY_COUNT)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::collections::BTreeSet;

    #[test]
    fn property_names_stay_bounded() {
        let names: BTreeSet<_> = (0..=u16::MAX).map(data_property_name).collect();
        assert_eq!(names.len(), DATA_PROPERTY_COUNT as usize);
    }
}
