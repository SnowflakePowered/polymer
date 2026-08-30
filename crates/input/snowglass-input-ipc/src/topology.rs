use snowglass_input_layout::topology::InputTopology;
use std::io;

pub fn encode(value: &InputTopology) -> io::Result<Vec<u8>> {
    bincode::serde::encode_to_vec(value, bincode::config::standard()).map_err(io::Error::other)
}
