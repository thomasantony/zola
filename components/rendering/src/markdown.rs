use lazy_static::lazy_static;
use pulldown_cmark as cmark;

use crate::context::RenderContext;
use crate::table_of_contents::{make_table_of_contents, Heading};
use errors::{Error, Result};
use front_matter::InsertAnchor;
use utils::site::resolve_internal_link;
use utils::slugs::slugify_anchors;
use utils::vec::InsertMany;

use self::cmark::{Event, LinkType, Options, Parser, Tag};
use crate::codeblock::{CodeBlock, FenceSettings};
use crate::shortcode::{Shortcode, SHORTCODE_PLACEHOLDER};

const CONTINUE_READING: &str = "<span id=\"continue-reading\"></span>";
const ANCHOR_LINK_TEMPLATE: &str = "anchor-link.html";

#[derive(Debug)]
pub struct Rendered {
    pub body: String,
    pub summary_len: Option<usize>,
    pub toc: Vec<Heading>,
    /// Links to site-local pages: relative path plus optional anchor target.
    pub internal_links: Vec<(String, Option<String>)>,
    /// Outgoing links to external webpages (i.e. HTTP(S) targets).
    pub external_links: Vec<String>,
}

/// Tracks a heading in a slice of pulldown-cmark events
#[derive(Debug)]
struct HeadingRef {
    start_idx: usize,
    end_idx: usize,
    level: u32,
    id: Option<String>,
}

impl HeadingRef {
    fn new(start: usize, level: u32) -> HeadingRef {
        HeadingRef { start_idx: start, end_idx: 0, level, id: None }
    }
}

// We might have cases where the slug is already present in our list of anchor
// for example an article could have several titles named Example
// We add a counter after the slug if the slug is already present, which
// means we will have example, example-1, example-2 etc
fn find_anchor(anchors: &[String], name: String, level: u16) -> String {
    if level == 0 && !anchors.contains(&name) {
        return name;
    }

    let new_anchor = format!("{}-{}", name, level + 1);
    if !anchors.contains(&new_anchor) {
        return new_anchor;
    }

    find_anchor(anchors, name, level + 1)
}

/// Returns whether a link starts with an HTTP(s) scheme.
fn is_external_link(link: &str) -> bool {
    link.starts_with("http:") || link.starts_with("https:")
}

fn fix_link(
    link_type: LinkType,
    link: &str,
    context: &RenderContext,
    internal_links: &mut Vec<(String, Option<String>)>,
    external_links: &mut Vec<String>,
) -> Result<String> {
    if link_type == LinkType::Email {
        return Ok(link.to_string());
    }

    // A few situations here:
    // - it could be a relative link (starting with `@/`)
    // - it could be a link to a co-located asset
    // - it could be a normal link
    let result = if link.starts_with("@/") {
        match resolve_internal_link(link, &context.permalinks) {
            Ok(resolved) => {
                internal_links.push((resolved.md_path, resolved.anchor));
                resolved.permalink
            }
            Err(_) => {
                return Err(format!("Relative link {} not found.", link).into());
            }
        }
    } else {
        if is_external_link(link) {
            external_links.push(link.to_owned());
        }
        link.to_string()
    };
    Ok(result)
}

/// get only text in a slice of events
fn get_text(parser_slice: &[Event]) -> String {
    let mut title = String::new();

    for event in parser_slice.iter() {
        match event {
            Event::Text(text) | Event::Code(text) => title += text,
            _ => continue,
        }
    }

    title
}

fn get_heading_refs(events: &[Event]) -> Vec<HeadingRef> {
    let mut heading_refs = vec![];

    for (i, event) in events.iter().enumerate() {
        match event {
            Event::Start(Tag::Heading(level)) => {
                heading_refs.push(HeadingRef::new(i, *level));
            }
            Event::End(Tag::Heading(_)) => {
                let msg = "Heading end before start?";
                heading_refs.last_mut().expect(msg).end_idx = i;
            }
            _ => (),
        }
    }

    heading_refs
}

pub fn markdown_to_html(
    content: &str,
    context: &RenderContext,
    html_shortcodes: Vec<Shortcode>,
) -> Result<Rendered> {
    lazy_static! {
        static ref EMOJI_REPLACER: gh_emoji::Replacer = gh_emoji::Replacer::new();
    }

    let path = context
        .tera_context
        .get("page")
        .or_else(|| context.tera_context.get("section"))
        .map(|x| x.as_object().unwrap().get("relative_path").unwrap().as_str().unwrap());
    // the rendered html
    let mut html = String::with_capacity(content.len());
    // Set while parsing
    let mut error = None;

    let mut code_block: Option<CodeBlock> = None;

    let mut inserted_anchors: Vec<String> = vec![];
    let mut headings: Vec<Heading> = vec![];
    let mut internal_links = Vec::new();
    let mut external_links = Vec::new();

    let mut stop_next_end_p = false;

    let mut opts = Options::empty();
    let mut has_summary = false;
    opts.insert(Options::ENABLE_TABLES);
    opts.insert(Options::ENABLE_FOOTNOTES);
    opts.insert(Options::ENABLE_STRIKETHROUGH);
    opts.insert(Options::ENABLE_TASKLISTS);

    if context.config.markdown.smart_punctuation {
        opts.insert(Options::ENABLE_SMART_PUNCTUATION);
    }

    // we reverse their order so we can pop them easily in order
    let mut html_shortcodes: Vec<_> = html_shortcodes.into_iter().rev().collect();
    let mut next_shortcode = html_shortcodes.pop();
    let contains_shortcode = |txt: &str| -> bool { txt.contains(SHORTCODE_PLACEHOLDER) };

    {
        let mut events = Vec::new();

        for (event, mut range) in Parser::new_ext(content, opts).into_offset_iter() {
            match event {
                Event::Text(text) => {
                    if let Some(ref mut code_block) = code_block {
                        let html = code_block.highlight(&text);
                        events.push(Event::Html(html.into()));
                    } else {
                        let text = if context.config.markdown.render_emoji {
                            EMOJI_REPLACER.replace_all(&text).to_string().into()
                        } else {
                            text
                        };

                        if !contains_shortcode(text.as_ref()) {
                            events.push(Event::Text(text));
                            continue;
                        }

                        // TODO: find a way to share that code with the HTML handler
                        let mut new_text = text.clone();
                        loop {
                            if let Some(ref shortcode) = next_shortcode {
                                let sc_span = shortcode.span.clone();
                                if range.contains(&sc_span.start) {
                                    if range.start != sc_span.start {
                                        events.push(Event::Text(
                                            new_text[..(sc_span.start - range.start)]
                                                .to_string()
                                                .into(),
                                        ));
                                    }

                                    let shortcode = next_shortcode.take().unwrap();

                                    match shortcode.render(&context.tera, &context.tera_context) {
                                        Ok(s) => {
                                            events.push(Event::Html(s.into()));
                                            new_text = new_text[(sc_span.end - range.start)..]
                                                .to_owned()
                                                .into();
                                            range.start = sc_span.end - range.start;
                                        }
                                        Err(e) => {
                                            error = Some(e);
                                            break;
                                        }
                                    }

                                    next_shortcode = html_shortcodes.pop();
                                    continue;
                                }
                            }

                            break;
                        }

                        events.push(Event::Text(new_text[..].to_string().into()));
                    }
                }
                Event::Start(Tag::CodeBlock(ref kind)) => {
                    let fence = match kind {
                        cmark::CodeBlockKind::Fenced(fence_info) => FenceSettings::new(fence_info),
                        _ => FenceSettings::new(""),
                    };
                    let (block, begin) = CodeBlock::new(fence, context.config, path);
                    code_block = Some(block);
                    events.push(Event::Html(begin.into()));
                }
                Event::End(Tag::CodeBlock(_)) => {
                    // reset highlight and close the code block
                    code_block = None;
                    events.push(Event::Html("</code></pre>\n".into()));
                }
                Event::Start(Tag::Link(link_type, link, title)) if link.is_empty() => {
                    error = Some(Error::msg("There is a link that is missing a URL"));
                    events.push(Event::Start(Tag::Link(link_type, "#".into(), title)));
                }
                Event::Start(Tag::Link(link_type, link, title)) => {
                    let fixed_link = match fix_link(
                        link_type,
                        &link,
                        context,
                        &mut internal_links,
                        &mut external_links,
                    ) {
                        Ok(fixed_link) => fixed_link,
                        Err(err) => {
                            error = Some(err);
                            events.push(Event::Html("".into()));
                            continue;
                        }
                    };

                    events.push(
                        if is_external_link(&link)
                            && context.config.markdown.has_external_link_tweaks()
                        {
                            let mut escaped = String::new();
                            // write_str can fail but here there are no reasons it should (afaik?)
                            cmark::escape::escape_href(&mut escaped, &link)
                                .expect("Could not write to buffer");
                            Event::Html(
                                context
                                    .config
                                    .markdown
                                    .construct_external_link_tag(&escaped, &title)
                                    .into(),
                            )
                        } else {
                            Event::Start(Tag::Link(link_type, fixed_link.into(), title))
                        },
                    )
                }
                Event::Start(Tag::Paragraph) => {
                    // We have to compare the start and the trimmed length because the content
                    // will sometimes contain '\n' at the end which we want to avoid.
                    //
                    // NOTE: It could be more efficient to remove this search and just keep
                    // track of the shortcodes to come and compare it to that.
                    if let Some(ref next_shortcode) = next_shortcode {
                        if next_shortcode.span.start == range.start
                            && next_shortcode.span.len() == content[range].trim().len()
                        {
                            stop_next_end_p = true;
                            events.push(Event::Html("".into()));
                            continue;
                        }
                    }

                    events.push(event);
                }
                Event::End(Tag::Paragraph) => {
                    events.push(if stop_next_end_p {
                        stop_next_end_p = false;
                        Event::Html("".into())
                    } else {
                        event
                    });
                }
                Event::Html(text) => {
                    if text.contains("<!-- more -->") {
                        has_summary = true;
                        events.push(Event::Html(CONTINUE_READING.into()));
                        continue;
                    }
                    if !contains_shortcode(text.as_ref()) {
                        events.push(Event::Html(text));
                        continue;
                    }

                    let mut new_text = text.clone();
                    loop {
                        if let Some(ref shortcode) = next_shortcode {
                            let sc_span = shortcode.span.clone();
                            if range.contains(&sc_span.start) {
                                if range.start != sc_span.start {
                                    events.push(Event::Html(
                                        new_text[..(sc_span.start - range.start)].to_owned().into(),
                                    ));
                                }

                                let shortcode = next_shortcode.take().unwrap();
                                match shortcode.render(&context.tera, &context.tera_context) {
                                    Ok(s) => {
                                        events.push(Event::Html(s.into()));
                                        new_text = new_text[(sc_span.end - range.start)..]
                                            .to_owned()
                                            .into();
                                        range.start = sc_span.end - range.start;
                                    }
                                    Err(e) => {
                                        error = Some(e);
                                        break;
                                    }
                                }

                                next_shortcode = html_shortcodes.pop();
                                continue;
                            }
                        }

                        break;
                    }
                    events.push(Event::Html(new_text[..].to_string().into()));
                }
                _ => events.push(event),
            }
        }

        // We remove all the empty things we might have pushed before so we don't get some random \n
        events = events
            .into_iter()
            .filter(|e| match e {
                Event::Text(text) | Event::Html(text) => !text.is_empty(),
                _ => true,
            })
            .collect();

        let mut heading_refs = get_heading_refs(&events);

        let mut anchors_to_insert = vec![];

        // First heading pass: look for a manually-specified IDs, e.g. `# Heading text {#hash}`
        // (This is a separate first pass so that auto IDs can avoid collisions with manual IDs.)
        for heading_ref in heading_refs.iter_mut() {
            let end_idx = heading_ref.end_idx;
            if let Event::Text(ref mut text) = events[end_idx - 1] {
                if text.as_bytes().last() == Some(&b'}') {
                    if let Some(mut i) = text.find("{#") {
                        let id = text[i + 2..text.len() - 1].to_owned();
                        inserted_anchors.push(id.clone());
                        while i > 0 && text.as_bytes()[i - 1] == b' ' {
                            i -= 1;
                        }
                        heading_ref.id = Some(id);
                        *text = text[..i].to_owned().into();
                    }
                }
            }
        }

        // Second heading pass: auto-generate remaining IDs, and emit HTML
        for heading_ref in heading_refs {
            let start_idx = heading_ref.start_idx;
            let end_idx = heading_ref.end_idx;
            let title = get_text(&events[start_idx + 1..end_idx]);
            let id = heading_ref.id.unwrap_or_else(|| {
                find_anchor(
                    &inserted_anchors,
                    slugify_anchors(&title, context.config.slugify.anchors),
                    0,
                )
            });
            inserted_anchors.push(id.clone());

            // insert `id` to the tag
            let html = format!("<h{lvl} id=\"{id}\">", lvl = heading_ref.level, id = id);
            events[start_idx] = Event::Html(html.into());

            // generate anchors and places to insert them
            if context.insert_anchor != InsertAnchor::None {
                let anchor_idx = match context.insert_anchor {
                    InsertAnchor::Left => start_idx + 1,
                    InsertAnchor::Right => end_idx,
                    InsertAnchor::None => 0, // Not important
                };
                let mut c = tera::Context::new();
                c.insert("id", &id);
                c.insert("level", &heading_ref.level);
                c.insert("lang", &context.lang);

                let anchor_link = utils::templates::render_template(
                    ANCHOR_LINK_TEMPLATE,
                    &context.tera,
                    c,
                    &None,
                )
                .map_err(|e| Error::chain("Failed to render anchor link template", e))?;
                anchors_to_insert.push((anchor_idx, Event::Html(anchor_link.into())));
            }

            // record heading to make table of contents
            let permalink = format!("{}#{}", context.current_page_permalink, id);
            let h =
                Heading { level: heading_ref.level, id, permalink, title, children: Vec::new() };
            headings.push(h);
        }

        if context.insert_anchor != InsertAnchor::None {
            events.insert_many(anchors_to_insert);
        }

        cmark::html::push_html(&mut html, events.into_iter());
    }

    if let Some(e) = error {
        Err(e)
    } else {
        Ok(Rendered {
            summary_len: if has_summary { html.find(CONTINUE_READING) } else { None },
            body: html,
            toc: make_table_of_contents(headings),
            internal_links,
            external_links,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_external_link() {
        assert!(is_external_link("http://example.com/"));
        assert!(is_external_link("https://example.com/"));
        assert!(is_external_link("https://example.com/index.html#introduction"));

        assert!(!is_external_link("mailto:user@example.com"));
        assert!(!is_external_link("tel:18008675309"));

        assert!(!is_external_link("#introduction"));

        assert!(!is_external_link("http.jpg"))
    }
}
