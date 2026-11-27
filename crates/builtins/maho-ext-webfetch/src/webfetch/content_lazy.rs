pub async fn html_to_markdown(html: &str, url: &str) -> String {
    super::content::html_to_markdown(html, url)
}

pub async fn html_to_text(html: &str, url: &str) -> String {
    super::content::html_to_text(html, url)
}

pub async fn looks_like_app_shell(html: &str, url: &str) -> bool {
    super::content::looks_like_app_shell(html, url)
}

pub async fn strip_scripts(html: &str, url: &str) -> String {
    super::content::strip_scripts(html, url)
}
