#[derive(Clone,Debug,PartialEq,Eq)]
pub enum GoalStoreError {
    GoalAlreadyExists(String),
    GoalNotFound(String),
    InvalidGoalStore(String),
    UnsupportedGoalStoreVersion(String),
    Io(String),
    Syntax(String),
    InvalidArgument(String),
}
impl GoalStoreError {
    pub const fn name(&self)->&'static str{match self{
        Self::GoalAlreadyExists(_)=>"GoalAlreadyExistsError",
        Self::GoalNotFound(_)=>"GoalNotFoundError",
        Self::InvalidGoalStore(_)=>"InvalidGoalStoreError",
        Self::UnsupportedGoalStoreVersion(_)=>"UnsupportedGoalStoreVersionError",
        Self::Io(_)=>"Error",
        Self::Syntax(_)=>"SyntaxError",
        Self::InvalidArgument(_)=>"Error",
    }}
}
impl std::fmt::Display for GoalStoreError{fn fmt(&self,f:&mut std::fmt::Formatter<'_>)->std::fmt::Result{match self{Self::GoalAlreadyExists(message)|Self::GoalNotFound(message)|Self::InvalidGoalStore(message)|Self::UnsupportedGoalStoreVersion(message)|Self::Io(message)|Self::Syntax(message)|Self::InvalidArgument(message)=>f.write_str(message)}}}
impl std::error::Error for GoalStoreError{}
impl From<GoalStoreError> for String{fn from(error:GoalStoreError)->Self{error.to_string()}}
impl From<String> for GoalStoreError{fn from(error:String)->Self{Self::InvalidArgument(error)}}
impl From<std::io::Error> for GoalStoreError{fn from(error:std::io::Error)->Self{Self::Io(error.to_string())}}
