#![no_main]

use hermes_harness::broker::live_capability_broker::{MAX_MESSAGE_BYTES, decode_request_for_fuzzing};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(fields) = decode_request_for_fuzzing(data) {
        let keys = [
            "capability",
            "task_id",
            "role",
            "session_id",
            "submission_id",
            "method",
            "text",
        ];
        assert!(data.len() <= MAX_MESSAGE_BYTES);
        assert_eq!(fields.len(), keys.len());
        assert!(keys.iter().all(|key| fields.contains_key(*key)));
        // Independently check the original document, including every value's type.
        let document = serde_json::from_slice::<serde_json::Value>(data);
        assert!(
            document.is_ok_and(|value| value.as_object().is_some_and(|object| {
                object.len() == keys.len()
                    && keys
                        .iter()
                        .all(|key| object.get(*key).is_some_and(serde_json::Value::is_string))
            }))
        );
    }
});
