use crate::memory::frame;

pub struct MemoryObject {
    frames: [Option<frame::Frame>; MAX_FRAMES],
    frame_count: usize,
}

const MAX_FRAMES: usize = 16;

impl MemoryObject {
    pub fn new(pages: usize) -> Option<Self> {
        if pages == 0 || pages > MAX_FRAMES {
            return None;
        }

        let mut frames = [None; MAX_FRAMES];

        for i in 0..pages {
            match frame::allocate() {
                Some(f) => frames[i] = Some(f),
                None => {
                    // Roll back anything already allocated.
                    for frame in frames.iter_mut().take(i) {
                        if let Some(f) = frame.take() {
                            frame::free(f);
                        }
                    }

                    return None;
                }
            }
        }

        Some(Self {
            frames,
            frame_count: pages,
        })
    }

    pub fn page_count(&self) -> usize {
        self.frame_count
    }
}

impl Drop for MemoryObject {
    fn drop(&mut self) {
        for frame in self.frames.iter_mut().take(self.frame_count) {
            if let Some(f) = frame.take() {
                frame::free(f);
            }
        }

        println!("Memory object destroyed.");
    }
}