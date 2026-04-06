use ratatui_image::picker::Picker;

fn main() {
    let mut picker = Picker::from_query_stdio().unwrap();
    println!("Protocol guessed: {:?}", picker.protocol_type());
}
