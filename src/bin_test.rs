use std::fs;
use serde_json::Value;

fn main() {
    let raw_text = fs::read_to_string("/tmp/payload_debug.json").unwrap();
    let json: Value = serde_json::from_str(&raw_text).unwrap();
    let mut count = 0;
    
    if let Some(items) = json["items"].as_array() {
        for (i, item) in items.iter().enumerate().take(3) {
            let mut track_obj = &item["track"];
            println!("Item {}: 'track' is_null: {}", i, track_obj.is_null());
            if track_obj.is_null() {
                track_obj = &item["item"];
                println!("Item {}: fallback 'item' is_null: {}", i, track_obj.is_null());
            }
            if track_obj.is_null() || !track_obj.is_object() {
                println!("Item {}: SKIPPED!", i);
                continue;
            }
            println!("Item {}: Name={}, URI={}", i, track_obj["name"], track_obj["uri"]);
            count += 1;
        }
    }
    println!("Total matched: {}", count);
}
