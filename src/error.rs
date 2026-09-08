use std::{borrow::Cow, fmt::Debug};

pub trait ScenarioError: Debug {
    fn kind(&self) -> Cow<'static, str>;
}
