//! Compiler-authenticated static CSS and preload resources.

use super::*;

pub(super) fn static_styles_from_values(
    values: Vec<witchy_interp::interpreter::CompilerValue>,
    pages: &[StaticPage],
) -> Result<Vec<StaticStyle>, WebCommandError> {
    use witchy_interp::interpreter::CompilerValue;

    let known_routes = pages
        .iter()
        .map(|page| page.route.as_str())
        .collect::<BTreeSet<_>>();
    let mut styles = Vec::with_capacity(values.len());
    let mut style_ids = BTreeSet::new();
    for value in values {
        let CompilerValue::Constructor { name, fields } = value else {
            return Err(fail("static style registry contains a non-style value"));
        };
        if name != "glamour.StaticStyle" || fields.len() != 3 {
            return Err(fail(format!(
                "static style registry contains `{name}` instead of `glamour.StaticStyle`"
            )));
        }
        let mut fields = fields.into_iter();
        let Some(CompilerValue::Constructor {
            name: sheet_name,
            fields: sheet_fields,
        }) = fields.next()
        else {
            return Err(fail("static style contains an invalid CSS sheet"));
        };
        if sheet_name != "glamour.CssSheet" || sheet_fields.len() != 5 {
            return Err(fail(format!(
                "static style contains `{sheet_name}` instead of `glamour.CssSheet`"
            )));
        }
        let mut sheet_fields = sheet_fields.into_iter();
        let id = compiler_string(sheet_fields.next(), "CSS sheet id")?;
        let scope = compiler_string(sheet_fields.next(), "CSS sheet scope")?;
        let origin = compiler_string(sheet_fields.next(), "CSS sheet origin")?;
        let text = compiler_string(sheet_fields.next(), "CSS sheet text")?;
        let classes = compiler_string_list(sheet_fields.next(), "CSS sheet class registry")?;
        validate_static_css(&id, &scope, &origin, &text, &classes)?;
        if !style_ids.insert(id.clone()) {
            return Err(fail(format!(
                "static style `{id}` is declared more than once"
            )));
        }
        let mut routes =
            compiler_string_list(fields.next(), "static style route registry")?;
        let mut critical_routes =
            compiler_string_list(fields.next(), "static style critical-route registry")?;
        validate_style_routes(&id, &mut routes, &known_routes, "route")?;
        validate_style_routes(
            &id,
            &mut critical_routes,
            &known_routes,
            "critical route",
        )?;
        if let Some(route) = critical_routes
            .iter()
            .find(|route| !routes.contains(route))
        {
            return Err(fail(format!(
                "static style `{id}` marks `{route}` critical without attaching the sheet to that route"
            )));
        }
        styles.push(StaticStyle {
            id,
            scope,
            origin,
            text,
            classes,
            routes,
            critical_routes,
        });
    }
    styles.sort_by(|left, right| left.id.cmp(&right.id));
    Ok(styles)
}

pub(super) fn static_preloads_from_values(
    values: Vec<witchy_interp::interpreter::CompilerValue>,
    pages: &[StaticPage],
) -> Result<Vec<StaticPreload>, WebCommandError> {
    use witchy_interp::interpreter::CompilerValue;

    let known_routes = pages
        .iter()
        .map(|page| page.route.as_str())
        .collect::<BTreeSet<_>>();
    let mut preloads = Vec::with_capacity(values.len());
    let mut unique = BTreeSet::new();
    for value in values {
        let CompilerValue::Constructor { name, fields } = value else {
            return Err(fail("static preload registry contains a non-preload value"));
        };
        if name != "glamour.StaticPreload" || fields.len() != 3 {
            return Err(fail(format!(
                "static preload registry contains `{name}` instead of `glamour.StaticPreload`"
            )));
        }
        let mut fields = fields.into_iter();
        let route = compiler_string(fields.next(), "static preload route")?;
        let href = compiler_string(fields.next(), "static preload URL")?;
        let kind = compiler_string(fields.next(), "static preload kind")?;
        if !known_routes.contains(route.as_str()) {
            return Err(fail(format!(
                "static preload references unknown route `{route}`"
            )));
        }
        if !valid_static_asset_path(&href) {
            return Err(fail(format!(
                "static preload on `{route}` has non-local or non-canonical asset URL `{href}`"
            )));
        }
        if !matches!(kind.as_str(), "style" | "font" | "image") {
            return Err(fail(format!(
                "static preload on `{route}` has unsupported kind `{kind}`"
            )));
        }
        if !unique.insert((route.clone(), href.clone())) {
            return Err(fail(format!(
                "static preload repeats `{href}` on route `{route}`"
            )));
        }
        preloads.push(StaticPreload { route, href, kind });
    }
    preloads.sort_by(|left, right| {
        (&left.route, &left.href, &left.kind).cmp(&(&right.route, &right.href, &right.kind))
    });
    Ok(preloads)
}

pub(super) fn static_assets_from_values(
    values: Vec<witchy_interp::interpreter::CompilerValue>,
) -> Result<Vec<StaticAsset>, WebCommandError> {
    use witchy_interp::interpreter::CompilerValue;

    let mut assets = Vec::with_capacity(values.len());
    let mut unique = BTreeSet::new();
    for value in values {
        let CompilerValue::Constructor { name, fields } = value else {
            return Err(fail("static asset registry contains a non-asset value"));
        };
        if name != "glamour.StaticAsset" || fields.len() != 1 {
            return Err(fail(format!(
                "static asset registry contains `{name}` instead of `glamour.StaticAsset`"
            )));
        }
        let href = compiler_string(fields.into_iter().next(), "static asset URL")?;
        if !valid_static_asset_path(&href) {
            return Err(fail(format!(
                "static asset has non-local or non-canonical URL `{href}`"
            )));
        }
        if !unique.insert(href.clone()) {
            return Err(fail(format!("static asset `{href}` is declared more than once")));
        }
        assets.push(StaticAsset { href });
    }
    assets.sort_by(|left, right| left.href.cmp(&right.href));
    Ok(assets)
}

pub(super) fn static_asset_sources(
    public: &Path,
    assets: &[StaticAsset],
) -> Result<BTreeSet<PathBuf>, WebCommandError> {
    assets
        .iter()
        .map(|asset| {
            let relative = PathBuf::from(asset.href.trim_start_matches('/'));
            validate_asset_source(public, &relative)?;
            Ok(relative)
        })
        .collect()
}

pub(super) fn publish_static_assets(
    public: &Path,
    destination: &Path,
    assets: &[StaticAsset],
) -> Result<BTreeMap<String, String>, WebCommandError> {
    let mut published = BTreeMap::new();
    for asset in assets {
        let relative = PathBuf::from(asset.href.trim_start_matches('/'));
        validate_asset_source(public, &relative)?;
        let source = public.join(&relative);
        let bytes = std::fs::read(&source).map_err(|error| {
            fail(format!(
                "cannot read typed public asset `{}`: {error}",
                source.display()
            ))
        })?;
        let stem = source
            .file_stem()
            .and_then(|value| value.to_str())
            .ok_or_else(|| fail(format!("typed public asset `{}` has no filename", asset.href)))?;
        let digest = &sha256(&bytes)[..16];
        let name = match source.extension().and_then(|value| value.to_str()) {
            Some(extension) => format!("{stem}-{digest}.{extension}"),
            None => format!("{stem}-{digest}"),
        };
        write_file(&destination.join(&name), &bytes)?;
        published.insert(asset.href.clone(), name);
    }
    Ok(published)
}

pub(super) fn validate_css_asset_bindings(
    styles: &[StaticStyle],
    assets: &[StaticAsset],
) -> Result<(), WebCommandError> {
    let declared = assets
        .iter()
        .map(|asset| asset.href.as_str())
        .collect::<BTreeSet<_>>();
    for style in styles {
        for href in css_asset_urls(&style.text)? {
            if !declared.contains(href.as_str()) {
                return Err(fail(format!(
                    "static CSS sheet `{}` references undeclared typed asset `{href}`",
                    style.id
                )));
            }
        }
    }
    Ok(())
}

pub(super) fn rewrite_static_css_assets(
    text: &str,
    assets: &BTreeMap<String, String>,
) -> Result<String, WebCommandError> {
    let mut rewritten = text.to_string();
    for href in css_asset_urls(text)? {
        let name = assets.get(&href).ok_or_else(|| {
            fail(format!(
                "static CSS references unpublished typed asset `{href}`"
            ))
        })?;
        rewritten = rewritten.replace(
            &format!("url(\"{href}\")"),
            &format!("url(\"/assets/{name}\")"),
        );
    }
    Ok(rewritten)
}

fn validate_asset_source(public: &Path, relative: &Path) -> Result<(), WebCommandError> {
    let mut current = public.to_path_buf();
    for (index, component) in relative.components().enumerate() {
        let Component::Normal(part) = component else {
            return Err(fail("typed public asset path is not canonical"));
        };
        current.push(part);
        let metadata = std::fs::symlink_metadata(&current).map_err(|error| {
            fail(format!(
                "typed public asset `{}` is missing: {error}",
                current.display()
            ))
        })?;
        if metadata.file_type().is_symlink() {
            return Err(fail(format!(
                "typed public asset `{}` crosses a symlink",
                current.display()
            )));
        }
        let last = index + 1 == relative.components().count();
        if (last && !metadata.is_file()) || (!last && !metadata.is_dir()) {
            return Err(fail(format!(
                "typed public asset `{}` must resolve to a regular file",
                current.display()
            )));
        }
    }
    Ok(())
}

pub(super) fn validate_emitted_preload(
    staging: &Path,
    preload: &StaticPreload,
) -> Result<(), WebCommandError> {
    let relative = preload.href.strip_prefix('/').ok_or_else(|| {
        fail(format!("static preload has invalid path `{}`", preload.href))
    })?;
    let asset = staging.join(relative);
    if !asset.is_file() {
        return Err(fail(format!(
            "static preload `{}` on route `{}` does not name an emitted public asset",
            preload.href, preload.route
        )));
    }
    let extension = asset
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let matches_kind = match preload.kind.as_str() {
        "style" => extension == "css",
        "font" => matches!(extension.as_str(), "woff" | "woff2"),
        "image" => matches!(
            extension.as_str(),
            "avif" | "gif" | "jpeg" | "jpg" | "png" | "svg" | "webp"
        ),
        _ => false,
    };
    if !matches_kind {
        return Err(fail(format!(
            "static preload `{}` does not match declared kind `{}`",
            preload.href, preload.kind
        )));
    }
    Ok(())
}

fn compiler_string(
    value: Option<witchy_interp::interpreter::CompilerValue>,
    label: &str,
) -> Result<String, WebCommandError> {
    use witchy_interp::interpreter::CompilerValue;

    match value {
        Some(CompilerValue::String(value)) => Ok(value),
        _ => Err(fail(format!("{label} is not text"))),
    }
}

fn compiler_string_list(
    value: Option<witchy_interp::interpreter::CompilerValue>,
    label: &str,
) -> Result<Vec<String>, WebCommandError> {
    use witchy_interp::interpreter::CompilerValue;

    let Some(CompilerValue::List(values)) = value else {
        return Err(fail(format!("{label} is not a closed list")));
    };
    values
        .into_iter()
        .map(|value| match value {
            CompilerValue::String(value) => Ok(value),
            _ => Err(fail(format!("{label} contains a non-text value"))),
        })
        .collect()
}

fn validate_style_routes(
    id: &str,
    routes: &mut [String],
    known_routes: &BTreeSet<&str>,
    label: &str,
) -> Result<(), WebCommandError> {
    let mut unique = BTreeSet::new();
    for route in routes.iter() {
        if !known_routes.contains(route.as_str()) {
            return Err(fail(format!(
                "static style `{id}` references unknown {label} `{route}`"
            )));
        }
        if !unique.insert(route.clone()) {
            return Err(fail(format!(
                "static style `{id}` repeats {label} `{route}`"
            )));
        }
    }
    routes.sort();
    Ok(())
}

fn validate_static_css(
    id: &str,
    scope: &str,
    origin: &str,
    text: &str,
    classes: &[String],
) -> Result<(), WebCommandError> {
    let digest = id
        .strip_prefix("glamour-css1-")
        .ok_or_else(|| fail(format!("static CSS sheet has invalid identity `{id}`")))?;
    let global = scope == "global";
    if !lower_hex(digest, 64) || (!global && !lower_hex(scope, 12)) {
        return Err(fail(format!(
            "static CSS sheet `{id}` has invalid identity or scope"
        )));
    }
    if origin.is_empty()
        || origin.len() > 512
        || origin
            .chars()
            .any(|character| character.is_control() && character != '\t')
    {
        return Err(fail(format!(
            "static CSS sheet `{id}` has invalid source metadata"
        )));
    }
    let mut declared = BTreeSet::new();
    for class in classes {
        if class.is_empty()
            || !class
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
            || !declared.insert(class.as_str())
        {
            return Err(fail(format!(
                "static CSS sheet `{id}` has an invalid or duplicate class `{class}`"
            )));
        }
    }
    let identity_input = format!("{scope}|{text}|{}", classes.join("|"));
    let expected = format!("glamour-css1-{}", sha256(identity_input.as_bytes()));
    if id != expected {
        return Err(fail(format!(
            "static CSS sheet `{id}` disagrees with its checked representation"
        )));
    }

    reject_unsafe_css(id, text)?;
    let prefix = format!("[data-glamour-scope=\"{scope}\"] ");
    let mut observed = BTreeSet::new();
    let mut rules = 0usize;
    for raw_rule in text.split('}') {
        let rule = raw_rule.trim();
        if rule.is_empty() {
            continue;
        }
        let Some((selectors, declarations)) = rule.split_once('{') else {
            return Err(fail(format!("static CSS sheet `{id}` contains a malformed rule")));
        };
        if declarations.contains('{')
            || declarations.trim().is_empty()
            || !declarations.contains(':')
        {
            return Err(fail(format!(
                "static CSS sheet `{id}` contains malformed declarations"
            )));
        }
        for selector in selectors.split(',') {
            let selector = selector.trim();
            let local = if global {
                selector
            } else {
                selector.strip_prefix(&prefix).ok_or_else(|| {
                    fail(format!(
                        "static CSS sheet `{id}` contains an unscoped selector"
                    ))
                })?
            };
            if local.is_empty() || local.contains('{') || local.contains('}') {
                return Err(fail(format!(
                    "static CSS sheet `{id}` contains an invalid selector"
                )));
            }
            collect_selector_classes(local, &mut observed);
        }
        rules += 1;
    }
    if rules == 0
        || observed.iter().any(|class| !declared.contains(*class))
        || declared.iter().any(|class| !observed.contains(*class))
    {
        return Err(fail(format!(
            "static CSS sheet `{id}` disagrees with its declared class registry"
        )));
    }
    Ok(())
}

fn lower_hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

fn reject_unsafe_css(id: &str, text: &str) -> Result<(), WebCommandError> {
    let lower = text.to_ascii_lowercase();
    css_asset_urls(text)?;
    if text.is_empty()
        || text.contains('@')
        || text.contains("/*")
        || text.contains("*/")
        || text.contains('\\')
        || lower.contains("javascript:")
        || lower.contains("expression(")
        || lower.contains("</style")
        || lower.contains("-moz-binding")
        || lower.contains("behavior:")
        || text
            .chars()
            .any(|character| character.is_control() && !matches!(character, '\n' | '\r' | '\t'))
    {
        return Err(fail(format!(
            "static CSS sheet `{id}` contains an unsupported or unsafe construct"
        )));
    }
    Ok(())
}

fn css_asset_urls(text: &str) -> Result<Vec<String>, WebCommandError> {
    let lower = text.to_ascii_lowercase();
    let mut urls = Vec::new();
    let mut cursor = 0;
    while let Some(found) = lower[cursor..].find("url") {
        let start = cursor + found;
        let after_name = start + 3;
        let suffix = &lower[after_name..];
        if !suffix.trim_start().starts_with('(') {
            cursor = after_name;
            continue;
        }
        if !text[start..].starts_with("url(\"") {
            return Err(fail(
                "static CSS asset references must use canonical `url(\"/path\")` syntax",
            ));
        }
        let value_start = start + 5;
        let Some(close) = text[value_start..].find("\")") else {
            return Err(fail("static CSS contains an unterminated asset reference"));
        };
        let value_end = value_start + close;
        let href = &text[value_start..value_end];
        if !valid_static_asset_path(href) {
            return Err(fail(format!(
                "static CSS contains non-local or non-canonical asset URL `{href}`"
            )));
        }
        urls.push(href.to_string());
        cursor = value_end + 2;
    }
    Ok(urls)
}

fn collect_selector_classes<'a>(selector: &'a str, classes: &mut BTreeSet<&'a str>) {
    let bytes = selector.as_bytes();
    let mut cursor = 0;
    while cursor < bytes.len() {
        if bytes[cursor] != b'.' {
            cursor += 1;
            continue;
        }
        let start = cursor + 1;
        let mut end = start;
        while end < bytes.len()
            && (bytes[end].is_ascii_alphanumeric() || matches!(bytes[end], b'-' | b'_'))
        {
            end += 1;
        }
        if end > start {
            classes.insert(&selector[start..end]);
        }
        cursor = end.max(cursor + 1);
    }
}

fn valid_static_asset_path(value: &str) -> bool {
    value.starts_with('/')
        && !value.starts_with("//")
        && !value.contains('\\')
        && !value.contains('%')
        && !value.contains('?')
        && !value.contains('#')
        && !value
            .split('/')
            .any(|segment| segment == "." || segment == "..")
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'-' | b'_' | b'.'))
        && value.len() > 1
}

fn fail(message: impl Into<String>) -> WebCommandError {
    WebCommandError::failure(message)
}
