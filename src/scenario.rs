use crate::error::ScenarioError;

pub trait Scenario: Default + Send + Sync + 'static {
    type Ctx: Send + Sync;
    type Error: ScenarioError;

    fn name() -> &'static str;

    fn run(ctx: &Self::Ctx) -> impl Future<Output = std::result::Result<(), Self::Error>>;

    fn validate(ctx: &Self::Ctx) -> impl Future<Output = bool>;
}
