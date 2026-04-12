#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Scale {
    C4 = 262,
    D4 = 294,
    D4S = 311,
    E4 = 330,
    F4 = 349,
    F4S = 370,
    G4 = 392,
    G4S = 415,
    A4 = 440,
    A4S = 466,
    B4 = 494,
    C5 = 523,
    C5S = 554,
    D5 = 587,
    D5S = 622,
    E5 = 659
}

pub struct Note {
    pub scale: Scale,
    pub div: u32,
    pub dot: bool,
}