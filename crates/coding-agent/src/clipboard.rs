use std::io::Write;

/// Copy text to the system clipboard.
///
/// Tries OSC 52 escape sequence first (works over SSH), then native clipboard.
pub fn copy_to_clipboard(text: &str) -> anyhow::Result<()> {
    // Try native clipboard via arboard
    match arboard::Clipboard::new() {
        Ok(mut clipboard) => {
            clipboard.set_text(text.to_string())?;
            return Ok(());
        }
        Err(e) => {
            tracing::warn!("arboard clipboard failed: {e}, falling back to OSC 52");
        }
    }

    // Fallback: OSC 52 escape sequence
    let encoded = base64_encode(text);
    let osc52 = format!("\x1b]52;c;{encoded}\x07");
    std::io::stdout().write_all(osc52.as_bytes())?;
    std::io::stdout().flush()?;
    Ok(())
}

/// Read text from the system clipboard.
pub fn read_from_clipboard() -> anyhow::Result<String> {
    let mut clipboard = arboard::Clipboard::new()?;
    Ok(clipboard.get_text()?)
}

fn base64_encode(s: &str) -> String {
    use std::io::Write;
    let mut encoder = base64_write_encoder::B64Encoder::new(Vec::new());
    encoder.write_all(s.as_bytes()).ok();
    let encoded = encoder.into_inner();
    String::from_utf8_lossy(&encoded).to_string()
}

// Minimal base64 encoder (no external dep needed)
mod base64_write_encoder {
    use std::io::Write;

    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

    pub struct B64Encoder<W: Write> {
        inner: W,
        buf: [u8; 3],
        buf_len: usize,
    }

    impl<W: Write> B64Encoder<W> {
        pub fn new(inner: W) -> Self {
            Self {
                inner,
                buf: [0; 3],
                buf_len: 0,
            }
        }

        pub fn into_inner(self) -> W {
            self.inner
        }

        fn flush_block(&mut self) -> std::io::Result<()> {
            if self.buf_len == 0 {
                return Ok(());
            }
            let b0 = self.buf[0];
            let b1 = if self.buf_len > 1 { self.buf[1] } else { 0 };
            let b2 = if self.buf_len > 2 { self.buf[2] } else { 0 };

            let out = [
                CHARS[(b0 >> 2) as usize],
                CHARS[(((b0 & 0x03) << 4) | (b1 >> 4)) as usize],
                if self.buf_len > 1 {
                    CHARS[(((b1 & 0x0F) << 2) | (b2 >> 6)) as usize]
                } else {
                    b'='
                },
                if self.buf_len > 2 {
                    CHARS[(b2 & 0x3F) as usize]
                } else {
                    b'='
                },
            ];
            self.inner.write_all(&out)?;
            self.buf_len = 0;
            Ok(())
        }
    }

    impl<W: Write> Write for B64Encoder<W> {
        fn write(&mut self, mut buf: &[u8]) -> std::io::Result<usize> {
            let total = buf.len();
            while !buf.is_empty() {
                let space = 3 - self.buf_len;
                let take = space.min(buf.len());
                self.buf[self.buf_len..self.buf_len + take].copy_from_slice(&buf[..take]);
                self.buf_len += take;
                buf = &buf[take..];
                if self.buf_len == 3 {
                    self.flush_block()?;
                }
            }
            Ok(total)
        }

        fn flush(&mut self) -> std::io::Result<()> {
            self.flush_block()?;
            self.inner.flush()
        }
    }
}
