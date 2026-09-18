//! Helper run inside the thumbnail sandbox: renders one image into a PNG with gdk-pixbuf, so
//! image decoders never run in the file manager process. With `--probe`, it prints what a
//! photo, a recording, a video or a document says about itself instead, for the details
//! panel. With `--decode`, it decodes a picture whole for the preview, and with `--play`
//! it decodes a video or a recording for the preview's player.

use std::process::ExitCode;

use spiral::gtk::gdk_pixbuf::Pixbuf;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if let [_, flag, kind, input] = args.as_slice()
        && flag == "--probe"
    {
        print!(
            "{}",
            spiral::metadata::probe(kind, std::path::Path::new(input))
        );
        return ExitCode::SUCCESS;
    }
    if let [_, flag, source, control, video, audio, formats] = args.as_slice()
        && flag == "--play"
    {
        let fd = |s: &String| s.parse().unwrap_or(-1);
        return match spiral::media::serve(source, fd(control), fd(video), fd(audio), formats) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("spiral-thumbnailer: {e}");
                ExitCode::FAILURE
            }
        };
    }
    if let [_, flag, input, output, orientation] = args.as_slice()
        && flag == "--decode"
    {
        let orientation = orientation.parse().unwrap_or(1);
        return match spiral::picture::decode(input.as_ref(), output.as_ref(), orientation) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("spiral-thumbnailer: {e}");
                ExitCode::FAILURE
            }
        };
    }
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
