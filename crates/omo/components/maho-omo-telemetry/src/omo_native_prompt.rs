pub fn occurrence_bucket(count:u64)->&'static str {match count {0|1=>"1",2=>"2",3..=5=>"3_5",_=>"6_plus"}}
pub fn length_bucket(length:usize)->&'static str {match length {0..100=>"lt_100",100..500=>"100_500",500..2000=>"500_2000",_=>"gte_2000"}}
pub fn ordinal_bucket(ordinal:u64)->&'static str {match ordinal {1=>"1",0|2|3=>"2_3",4..=10=>"4_10",11..=25=>"11_25",_=>"26_plus"}}
pub fn queue_mode(disposition:Option<&str>,streaming:Option<&str>)->&'static str {match (disposition,streaming) {(Some("started"),_)=>"immediate",(Some("queued"),Some("followUp"))=>"follow_up",(Some("queued"),Some("steer"))=>"steer",_=>"other"}}
#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn occurrences() {for (n,want) in [(0,"1"),(1,"1"),(2,"2"),(3,"3_5"),(5,"3_5"),(6,"6_plus")] {assert_eq!(occurrence_bucket(n),want);}}
    #[test] fn lengths() {for (n,want) in [(99,"lt_100"),(100,"100_500"),(499,"100_500"),(500,"500_2000"),(1999,"500_2000"),(2000,"gte_2000")] {assert_eq!(length_bucket(n),want);}}
    #[test] fn ordinals() {for (n,want) in [(1,"1"),(2,"2_3"),(3,"2_3"),(4,"4_10"),(10,"4_10"),(11,"11_25"),(25,"11_25"),(26,"26_plus")] {assert_eq!(ordinal_bucket(n),want);}}
    #[test] fn queues() {assert_eq!(queue_mode(Some("started"),None),"immediate");assert_eq!(queue_mode(Some("queued"),Some("steer")),"steer");assert_eq!(queue_mode(Some("queued"),Some("followUp")),"follow_up");assert_eq!(queue_mode(Some("rejected"),Some("steer")),"other");}
}
