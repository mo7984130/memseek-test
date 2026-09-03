use std::marker::PhantomData;

use crate::scenario::Scenario;

pub struct ScenarioRegistry<S> {
    _marker: PhantomData<S>,
}

impl<S> ScenarioRegistry<S> {
    pub async fn run_all<C, E>(ctx: &C) -> Result<(), E>
    where
        S: ScenarioSet<C, E>,
    {
        S::run_all(ctx).await
    }
}

pub trait ScenarioSet<C, E> {
    fn run_all(ctx: &C) -> impl Future<Output = Result<(), E>>;
}

impl<A, B, C, E> ScenarioSet<C, E> for (A, B)
where
    A: Scenario<Ctx = C, Error = E>,
    B: Scenario<Ctx = C, Error = E>,
{
    async fn run_all(ctx: &C) -> Result<(), E> {
        A::run(ctx).await?;
        B::run(ctx).await?;

        Ok(())
    }
}
