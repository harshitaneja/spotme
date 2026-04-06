use ratatui_image::picker::Picker;

fn main() {
    match Picker::from_query_stdio() {
        Ok(picker) => {
            println!("SUCCESS!");
            println!("Protocol Guessed: {:?}", picker.protocol_type());
            println!("Font Size Detected: {:?}", picker.font_size());
        }
        Err(e) => {
            println!("FAILED TO QUERY TERMINAL: {:?}", e);
        }
    }
}
