pub async fn html_to_markdown(html: &str, url: &str) -> String {
    super::content::html_to_markdown(html, url)
}

pub async fn html_to_text(html: &str, url: &str) -> String {
    super::content::html_to_text(html, url)
}
