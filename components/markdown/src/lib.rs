mod codeblock;
mod context;
mod markdown;
mod shortcode;

use shortcode::{extract_shortcodes, insert_md_shortcodes};

use errors::Result;

use crate::markdown::markdown_to_html;
pub use crate::markdown::Rendered;
pub use context::RenderContext;

use libs::regex;
use libs::regex::RegexBuilder;

fn parse_wiki_style_links(content: &str) -> String {
    let regex_link_only = RegexBuilder::new(r"\[\[([^|]+)\]\]").build().unwrap();
    let regex_link_with_name = RegexBuilder::new(r"\[\[([^|]+)(|[^\]]+?)?\]\]").build().unwrap();
    let foo = regex_link_only.replace_all(content, |caps: &regex::Captures| {
        format!("[{}](@/{})", &caps[1].trim(), &caps[1].trim())
    });
    regex_link_with_name
        .replace_all(&foo.to_string(), |caps: &regex::Captures| {
            let mut title = caps[2].trim().to_string();
            title.remove(0); // Remove the "|"
            format!("[{}](@/{})", title.trim(), &caps[1].trim())
        })
        .to_string()
}
pub fn render_content(content: &str, context: &RenderContext) -> Result<markdown::Rendered> {
    let content_str = parse_wiki_style_links(content);
    let content = &content_str;
    // avoid parsing the content if needed
    if !content.contains("{{") && !content.contains("{%") {
        return markdown_to_html(content, context, Vec::new());
    }

    let definitions = context.shortcode_definitions.as_ref();
    // Extract all the defined shortcodes
    let (content, shortcodes) = extract_shortcodes(content, definitions)?;

    // Step 1: we render the MD shortcodes before rendering the markdown so they can get processed
    let (content, html_shortcodes) =
        insert_md_shortcodes(content, shortcodes, &context.tera_context, &context.tera)?;

    // Step 2: we render the markdown and the HTML markdown at the same time
    let html_context = markdown_to_html(&content, context, html_shortcodes)?;

    // TODO: Here issue #1418 could be implemented
    // if do_warn_about_unprocessed_md {
    //     warn_about_unprocessed_md(unprocessed_md);
    // }

    Ok(html_context)
}

#[cfg(test)]
pub mod tests {
    use super::*;
    #[test]
    fn test_parse_wiki_style_links() {
        assert_eq!(parse_wiki_style_links("[[about|About]]"), "[About](@/about)");
        assert_eq!(parse_wiki_style_links("[[about]]"), "[about](@/about)");
    }
}
