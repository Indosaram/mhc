#[derive(Clone,Debug,PartialEq,Eq)]
pub enum GoalStoreError {
    GoalAlreadyExists(String),
    GoalNotFound(String),
    InvalidGoalStore(String),
    UnsupportedGoalStoreVersion(String),
}
impl GoalStoreError {
    pub const fn name(&self)->&'static str{match self{
        Self::GoalAlreadyExists(_)=>"GoalAlreadyExistsError",
        Self::GoalNotFound(_)=>"GoalNotFoundError",
        Self::InvalidGoalStore(_)=>"InvalidGoalStoreError",
        Self::UnsupportedGoalStoreVersion(_)=>"UnsupportedGoalStoreVersionError",
    }}
}
impl std::fmt::Display for GoalStoreError{fn fmt(&self,f:&mut std::fmt::Formatter<'_>)->std::fmt::Result{match self{Self::GoalAlreadyExists(message)|Self::GoalNotFound(message)|Self::InvalidGoalStore(message)|Self::UnsupportedGoalStoreVersion(message)=>f.write_str(message)}}}
impl std::error::Error for GoalStoreError{}
