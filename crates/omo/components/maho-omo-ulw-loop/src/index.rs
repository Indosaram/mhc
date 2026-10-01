pub fn status_has_active_incomplete_run(value:&serde_json::Value)->bool {
    if value["ok"]!=true || !value["plan"].is_object() || value["plan"]["aggregateCompletion"]["status"]=="complete" { return false; }
    value["plan"]["goals"].as_array().is_some_and(|goals|goals.iter().any(|goal| {
        if !goal.is_object() || matches!(goal["steeringStatus"].as_str(),Some("superseded"|"blocked")) || !matches!(goal["status"].as_str(),Some("pending"|"in_progress")) { return false; }
        goal["successCriteria"].as_array().is_none_or(|criteria|criteria.is_empty() || criteria.iter().any(|c|c["status"]!="pass"))
    }))
}
