use crate::facts_runner_types::FactsLaunchResult;
pub async fn drain_facts_launches<E,F:std::future::Future<Output=Result<FactsLaunchResult,E>>>(mut attempt:impl FnMut()->F,aborted:impl Fn()->bool)->Result<FactsLaunchResult,E>{let first=attempt().await?;let mut latest=first.clone();while matches!(latest,FactsLaunchResult::Committed{..}|FactsLaunchResult::NoFacts{..})&&!aborted(){latest=attempt().await?;}Ok(first)}
#[cfg(test)]
mod tests{
    use super::*;
    use std::cell::Cell;
    fn committed()->FactsLaunchResult{FactsLaunchResult::Committed{run_id:"facts-1".into(),sha:"aaa".into()}}
    #[tokio::test]async fn successful_batches_continue_and_return_first(){let outcomes=[committed(),FactsLaunchResult::NoFacts{run_id:"facts-2".into()},committed(),FactsLaunchResult::Empty];let mut calls=0;let result=drain_facts_launches(||{let result=outcomes[calls].clone();calls+=1;std::future::ready(Ok::<_,()>(result))},||false).await.unwrap();assert_eq!(calls,4);assert_eq!(result,committed());}
    #[tokio::test]async fn failure_active_and_skip_stop_drain(){for stop in [FactsLaunchResult::Failed{run_id:"facts-2".into()},FactsLaunchResult::ParentDirty{run_id:"facts-2".into()},FactsLaunchResult::Active,FactsLaunchResult::Skipped]{let mut calls=0;let result=drain_facts_launches(||{calls+=1;std::future::ready(Ok::<_,()>(if calls==1{committed()}else{stop.clone()}))},||false).await.unwrap();assert_eq!(calls,2);assert_eq!(result,committed());}}
    #[tokio::test]async fn cancellation_prevents_followup_not_first_attempt(){let aborted=Cell::new(false);let mut calls=0;let result=drain_facts_launches(||{calls+=1;aborted.set(true);std::future::ready(Ok::<_,()>(committed()))},||aborted.get()).await.unwrap();assert_eq!(calls,1);assert_eq!(result,committed());}
    #[tokio::test]async fn attempt_errors_propagate(){assert_eq!(drain_facts_launches(||std::future::ready(Err::<FactsLaunchResult,_>("launch")),||false).await,Err("launch"));}
}
