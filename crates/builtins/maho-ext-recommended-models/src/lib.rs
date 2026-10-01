use maho_ai::{model::Model,types::ThinkingLevel};

pub const RECOMMENDED_DEFAULT_MODELS: &[(&str,ThinkingLevel)] = &[
    ("kimi-k3",ThinkingLevel::Max),("gpt-6-astra",ThinkingLevel::High),
    ("gpt-6-sol",ThinkingLevel::Medium),("gpt-5.6-sol",ThinkingLevel::Medium),
    ("claude-fable-5-1",ThinkingLevel::High),("claude-opus-5-5",ThinkingLevel::Max),
    ("glm-5.2",ThinkingLevel::Max),
];
pub fn canonical_model_id(id:&str)->String{
    let mut id=id.to_lowercase();
    while let Some(suffix)=["-ultrafast","-unlocked","-256k","-fast"].iter().find(|suffix|id.ends_with(**suffix)){
        id.truncate(id.len()-suffix.len());
    }
    if id=="k3"{"kimi-k3".into()}else{id}
}
#[derive(Clone,Debug,PartialEq,Eq)]
pub struct Recommendation{pub model_id:String,pub thinking_level:ThinkingLevel}
pub fn recommendations_for(configured:Option<&[String]>)->Vec<Recommendation>{
    configured.map_or_else(||RECOMMENDED_DEFAULT_MODELS.iter().map(|(id,level)|Recommendation{model_id:(*id).into(),thinking_level:*level}).collect(),|ids|{
        ids.iter().map(|id|canonical_model_id(id)).filter(|id|!id.is_empty()).map(|model_id|{
            let thinking_level=RECOMMENDED_DEFAULT_MODELS.iter().find(|(id,_)|*id==model_id).map_or(ThinkingLevel::Medium,|(_,level)|*level);
            Recommendation{model_id,thinking_level}
        }).collect()
    })
}
pub fn is_recommended(model:Option<&Model>,recommendations:&[Recommendation])->bool{
    model.is_some_and(|model|recommendations.iter().any(|recommendation|recommendation.model_id==canonical_model_id(&model.id)))
}
pub fn find_available_recommendation<'a>(recommendations:&'a [Recommendation],available:&'a [Model],has_auth:impl Fn(&Model)->bool)->Option<(&'a Recommendation,&'a Model)>{
    for recommendation in recommendations{
        if let Some(model)=available.iter().find(|model|has_auth(model)&&canonical_model_id(&model.id)==recommendation.model_id){return Some((recommendation,model));}
    }
    None
}
pub fn can_auto_switch(mode:&str,provenance:Option<&str>)->bool{mode!="app-server"&&matches!(provenance,Some("provider-default"|"first-available"))}

