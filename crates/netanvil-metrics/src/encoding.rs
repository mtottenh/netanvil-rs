use hdrhistogram::serialization::Serializer;
use hdrhistogram::Histogram;
use netanvil_types::NetAnvilError;

/// Serialize an HDR histogram to V2 compressed format.
pub fn encode_histogram(hist: &Histogram<u64>) -> Result<Vec<u8>, NetAnvilError> {
    let mut buf = Vec::new();
    hdrhistogram::serialization::V2Serializer::new()
        .serialize(hist, &mut buf)
        .map_err(|e| NetAnvilError::Other(format!("histogram encode: {e}")))?;
    Ok(buf)
}

/// Deserialize an HDR histogram from V2 compressed format.
pub fn decode_histogram(bytes: &[u8]) -> Result<Histogram<u64>, NetAnvilError> {
    let mut deserializer = hdrhistogram::serialization::Deserializer::new();
    let hist: Histogram<u64> = deserializer
        .deserialize(&mut std::io::Cursor::new(bytes))
        .map_err(|e| NetAnvilError::Other(format!("histogram decode: {e}")))?;
    Ok(hist)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_decode_roundtrip() {
        let mut hist = Histogram::<u64>::new_with_bounds(1, 60_000_000_000, 3).unwrap();
        hist.record(1_000_000).unwrap(); // 1ms
        hist.record(5_000_000).unwrap(); // 5ms
        hist.record(100_000_000).unwrap(); // 100ms

        let bytes = encode_histogram(&hist).unwrap();
        let decoded = decode_histogram(&bytes).unwrap();

        assert_eq!(decoded.len(), 3);
        assert_eq!(hist.value_at_quantile(0.5), decoded.value_at_quantile(0.5));
        assert_eq!(
            hist.value_at_quantile(0.99),
            decoded.value_at_quantile(0.99)
        );
    }
}
