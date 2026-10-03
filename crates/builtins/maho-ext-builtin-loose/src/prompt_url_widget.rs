use serde_json::Value;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PromptKind { Pr, Issue }

#[derive(Debug, PartialEq, Eq)]
pub struct PromptMatch<'a> { pub kind: PromptKind, pub url: &'a str }

pub fn extract_prompt_match(prompt: &str) -> Option<PromptMatch<'_>> {
    for (prefix, kind) in [
        ("You are given one or more GitHub PR URLs:", PromptKind::Pr),
        ("Analyze GitHub issue(s):", PromptKind::Issue),
    ] {
        for line in prompt.lines() {
            let line = line.trim_start();
            if line.len() >= prefix.len() && line.get(..prefix.len()).is_some_and(|start| start.eq_ignore_ascii_case(prefix))
                && let Some(url) = line[prefix.len()..].split_whitespace().next() {
                return Some(PromptMatch { kind, url });
            }
        }
    }
    None
}

pub fn format_author(author: Option<&Value>) -> Option<String> {
    let author = author?;
    let name = author.get("name").and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty());
    let login = author.get("login").and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty());
    match (name, login) {
        (Some(name), Some(login)) => Some(format!("{name} (@{login})")),
        (None, Some(login)) => Some(format!("@{login}")),
        (Some(name), None) => Some(name.to_owned()),
        (None, None) => None,
    }
}

pub fn desired_session_name(found: &PromptMatch<'_>, title: Option<&str>, current: Option<&str>) -> Option<String> {
    let label = match found.kind { PromptKind::Pr => "PR", PromptKind::Issue => "Issue" };
    let fallback = format!("{label}: {}", found.url);
    let current = current.map(str::trim).unwrap_or("");
    if !current.is_empty() && current != found.url && current != fallback { return None; }
    Some(match title.map(str::trim).filter(|title| !title.is_empty()) {
        Some(title) => format!("{label}: {title} ({})", found.url),
        None => fallback,
    })
}

use maho_ext_api::*;
use std::sync::{Arc,Mutex};
pub struct PromptUrlWidget;
fn user_text(content:&Value)->String{
    content.as_str().map(str::to_owned).unwrap_or_else(||content.as_array().map_or_else(String::new,|parts|parts.iter().filter(|part|part["type"]=="text").filter_map(|part|part["text"].as_str()).collect::<Vec<_>>().join("\n")))
}
fn display(ctx:&ExtensionContext,sender:&ExtensionApi,kind:PromptKind,url:&str,metadata:Option<&Value>)->Result<(),ExtensionFailure>{
    let title=metadata.and_then(|metadata|metadata["title"].as_str()).map(str::trim).filter(|title|!title.is_empty());
    let author=format_author(metadata.and_then(|metadata|metadata.get("author")));
    let spec=maho_ext_host::notice::NoticeSpec{title:title.unwrap_or(url).into(),tone:Some(maho_ext_host::notice::NoticeTone::Accent),why:author.unwrap_or_else(||"Prompt URL detected.".into()),extra:vec![maho_ext_host::notice::NoticeLine{text:url.into(),tone:Some(maho_ext_host::notice::NoticeTone::Dim)}],expanded_line:None};
    ctx.ui.set_widget("prompt-url",Some(WidgetContent::Component(Arc::new(move|theme|maho_ext_host::notice::build_notice_box(spec.clone(),false,theme)))),Default::default());
    if let Some(name)=desired_session_name(&PromptMatch{kind,url},title,sender.get_session_name()?.as_deref()){sender.set_session_name(&name)?;}
    Ok(())
}
impl Extension for PromptUrlWidget{
    fn register(&self,api:&mut ExtensionApi){
        let sender=Arc::new(ExtensionApi::new(api.registered.clone(),api.profile.clone(),api.events.clone(),api.runtime.clone()));
        let pending=Arc::new(Mutex::new(Vec::<tokio::task::JoinHandle<()>>::new()));
        for kind in [EventKind::BeforeAgentStart,EventKind::SessionStart]{
            let sender=sender.clone();let pending=pending.clone();
            api.on(kind,Arc::new(move|event,ctx|{let sender=sender.clone();let pending=pending.clone();Box::pin(async move{
                if !ctx.has_ui{return Ok(EventResult::None);}
                let prompt=match event{
                    ExtensionEvent::BeforeAgentStart(event)=>event.prompt.clone(),
                    _=>ctx.session_manager.get_entries().into_iter().rev().filter(|entry|entry.data["type"]=="message"&&entry.data["message"]["role"]=="user").map(|entry|user_text(&entry.data["message"]["content"])).find(|text|extract_prompt_match(text).is_some()).unwrap_or_default(),
                };
                let Some(found)=extract_prompt_match(&prompt)else{if kind==EventKind::SessionStart{ctx.ui.set_widget("prompt-url",None,Default::default());}return Ok(EventResult::None)};
                let url=found.url.to_owned();let found_kind=found.kind;display(ctx,&sender,found_kind,&url,None)?;
                let owner=ctx.clone();let task=tokio::spawn(async move{
                    let args=vec![if found_kind==PromptKind::Pr{"pr"}else{"issue"}.into(),"view".into(),url.clone(),"--json".into(),"title,author".into()];
                    let metadata=sender.exec("gh",&args,ExecOptions::default()).await.ok().filter(|result|result.code==0).and_then(|result|serde_json::from_str::<Value>(&result.stdout).ok());
                    if let Err(error)=display(&owner,&sender,found_kind,&url,metadata.as_ref()){owner.ui.notify(&error.message,NotificationType::Error);}
                });
                pending.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(task);
                Ok(EventResult::None)
            })}));
        }
        api.on(EventKind::SessionShutdown,Arc::new(move|_,ctx|{let pending=pending.clone();Box::pin(async move{
            let tasks=std::mem::take(&mut *pending.lock().unwrap_or_else(std::sync::PoisonError::into_inner));for task in &tasks{task.abort();}for task in tasks{let _result=task.await;}ctx.ui.set_widget("prompt-url",None,Default::default());Ok(EventResult::None)
        })}));
    }
}
