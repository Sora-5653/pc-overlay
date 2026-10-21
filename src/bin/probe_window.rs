use pc_overlay_core::WindowLocator;

fn main() {
    match WindowLocator::default().locate() {
        Ok(Some(window)) => println!(
            "found: title={:?}, pid={}, handle={}, client=({},{} {}x{})",
            window.title,
            window.process_id,
            window.handle,
            window.client_rect.left,
            window.client_rect.top,
            window.client_rect.width,
            window.client_rect.height
        ),
        Ok(None) => println!("not found: no qualifying visible TETR.IO window"),
        Err(error) => eprintln!("window discovery failed: {error}"),
    }
}
