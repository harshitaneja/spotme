use serde_json::Value;

fn main() {
    let raw_text = std::fs::read_to_string("/tmp/payload_debug.json").unwrap();
    let json: Value = serde_json::from_str(&raw_text).unwrap();
    if let Some(items) = json["items"].as_array() {
        for (i, item) in items.iter().enumerate().take(2) {
            let mut track_obj = &item["track"];
            let is_null_track = track_obj.is_null();
            let mut used_fallback = false;
            if track_obj.is_null() {
                track_obj = &item["item"];
                used_fallback = true;
            }
            let is_null_final = track_obj.is_null();
            let is_obj = track_obj.is_object();
            
            println!("Element {}", i);
            println!("  'track' was null? {}", is_null_track);
            println!("  fallback triggered? {}", used_fallback);
            println!("  final is_null? {}", is_null_final);
            println!("  final is_obj? {}", is_obj);
            if is_null_final || !is_obj {
                println!("  => SKIPPING!");
            } else {
                println!("  => SUCCESS: name = {}", track_obj["name"]);
            }
        }
    }
}
