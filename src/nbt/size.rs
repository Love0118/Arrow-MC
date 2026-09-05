use super::write::{list_element_type, modified_utf8_len};
use super::{CompoundEntry, Error, Limits, NbtString, Tag};

/// Exact byte length of one network NBT root, including its type byte. Uses no
/// heap allocation and at most 512 fixed traversal frames. Format, depth and
/// output-limit errors follow write_network's order; a later output allocation
/// can still independently fail when the caller admits and writes the packet.
/// Decoding-only allocation/quota limits do not affect either sizing or writing.
pub fn network_encoded_len(tag: &Tag, limits: Limits) -> Result<usize, Error> {
    limits.validate()?;
    let mut size = Size { bytes: 0, limits };
    size.add(1)?;
    let mut frames = [Frame::Empty; 512];
    let mut frame_count = 0;
    let mut current = Some((tag, 0));
    loop {
        if let Some((tag, depth)) = current.take() {
            match tag {
                Tag::End => {}
                Tag::Byte(_) => size.add(1)?,
                Tag::Short(_) => size.add(2)?,
                Tag::Int(_) | Tag::Float(_) => size.add(4)?,
                Tag::Long(_) | Tag::Double(_) => size.add(8)?,
                Tag::ByteArray(values) => {
                    size.length(values.len())?;
                    size.add(values.len())?;
                }
                Tag::IntArray(values) => {
                    size.length(values.len())?;
                    size.add(values.len().checked_mul(4).ok_or(Error::LengthOverflow)?)?;
                }
                Tag::LongArray(values) => {
                    size.length(values.len())?;
                    size.add(values.len().checked_mul(8).ok_or(Error::LengthOverflow)?)?;
                }
                Tag::String(value) => size.string(value)?,
                Tag::List(values) => {
                    let child_depth = size.container(depth)?;
                    let raw_type = list_element_type(values)?;
                    size.add(1)?;
                    size.length(values.len())?;
                    frames[frame_count] = Frame::List {
                        remaining: values,
                        child_depth,
                        raw_type,
                        close_wrapper: false,
                    };
                    frame_count += 1;
                }
                Tag::Compound(compound) => {
                    let child_depth = size.container(depth)?;
                    frames[frame_count] = Frame::Compound {
                        remaining: compound.entries(),
                        child_depth,
                    };
                    frame_count += 1;
                }
            }
        }
        while frame_count != 0 {
            match &mut frames[frame_count - 1] {
                Frame::Compound {
                    remaining,
                    child_depth,
                } => {
                    if let Some((entry, tail)) = remaining.split_first() {
                        if matches!(entry.value, Tag::End) {
                            return Err(Error::UnexpectedEnd);
                        }
                        size.add(1)?;
                        size.string(&entry.name)?;
                        *remaining = tail;
                        current = Some((&entry.value, *child_depth));
                        break;
                    }
                    size.add(1)?;
                    frame_count -= 1;
                }
                Frame::List {
                    remaining,
                    child_depth,
                    raw_type,
                    close_wrapper,
                } => {
                    if *close_wrapper {
                        size.add(1)?;
                        *close_wrapper = false;
                    }
                    if let Some((value, tail)) = remaining.split_first() {
                        *remaining = tail;
                        if *raw_type == 10
                            && !matches!(value, Tag::Compound(compound) if !compound.is_wrapper())
                        {
                            let wrapped_depth = size.container(*child_depth)?;
                            size.add(1)?;
                            size.add(2)?;
                            *close_wrapper = true;
                            current = Some((value, wrapped_depth));
                        } else {
                            current = Some((value, *child_depth));
                        }
                        break;
                    }
                    frame_count -= 1;
                }
                Frame::Empty => unreachable!("only active container frames are visited"),
            }
        }
        if current.is_none() {
            return Ok(size.bytes);
        }
    }
}

struct Size {
    bytes: usize,
    limits: Limits,
}

impl Size {
    fn add(&mut self, bytes: usize) -> Result<(), Error> {
        self.bytes = self.bytes.checked_add(bytes).ok_or(Error::OutputLimit)?;
        if self.bytes > self.limits.output_bytes {
            Err(Error::OutputLimit)
        } else {
            Ok(())
        }
    }
    fn length(&mut self, count: usize) -> Result<(), Error> {
        i32::try_from(count).map_err(|_| Error::LengthOverflow)?;
        self.add(4)
    }
    fn string(&mut self, value: &NbtString) -> Result<(), Error> {
        self.add(
            modified_utf8_len(value)?
                .checked_add(2)
                .ok_or(Error::LengthOverflow)?,
        )
    }
    fn container(&self, depth: usize) -> Result<usize, Error> {
        if depth >= self.limits.max_depth {
            Err(Error::DepthLimit)
        } else {
            Ok(depth + 1)
        }
    }
}

#[derive(Clone, Copy)]
enum Frame<'a> {
    Empty,
    Compound {
        remaining: &'a [CompoundEntry],
        child_depth: usize,
    },
    List {
        remaining: &'a [Tag],
        child_depth: usize,
        raw_type: u8,
        close_wrapper: bool,
    },
}
