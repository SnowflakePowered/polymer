pub enum InstallableType {
    App,
    System,
}

pub trait Installable {
    fn installable_type(&self) -> InstallableType;
}

pub trait Artifact {
    type Package: Installable;
}