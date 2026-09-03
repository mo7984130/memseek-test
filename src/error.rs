use std::fmt::Debug;

pub trait ScenarioError: Debug {
    fn kind(&self) -> &'static str;
}
