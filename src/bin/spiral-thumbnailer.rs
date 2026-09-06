//! Helper run inside the thumbnail sandbox: renders one image into a PNG with gdk-pixbuf, so
//! image decoders never run in the file manager process.

use std::process::ExitCode;

use spiral::gtk::gdk_pixbuf::Pixbuf;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let [_, input, output, size] = args.as_slice() else {
        eprintln!("usage: spiral-thumbnailer INPUT OUTPUT SIZE");
        return ExitCode::from(2);
    };
    let size: i32 = size.parse().unwrap_or(256);
    let result = Pixbuf::from_file_at_scale(input, size, size, true).and_then(|pb| {
        let pb = pb.apply_embedded_orientation().unwrap_or(pb);
        pb.savev(output, "png", &[])
    });
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("spiral-thumbnailer: {e}");
            ExitCode::FAILURE
        }
    }
}
