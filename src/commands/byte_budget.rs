use crate::channel::SkippedDetail;
use crate::error::{AppError, AppResult, ErrorCode};
use serde::Serialize;

pub(super) struct PrefixAdmission {
    pub(super) count: usize,
    pub(super) rendered: String,
}

/// Exact replacement cost for prefixes of one JSON array. Each item is
/// serialized once; later prefix probes are O(1) and never revisit bodies.
pub(super) struct JsonArrayPrefix {
    extra_bytes: Vec<usize>,
    extra_lines: Vec<usize>,
}

impl JsonArrayPrefix {
    pub(super) fn new<T: Serialize>(
        items: &[T],
        pretty: bool,
        item_indent: usize,
    ) -> AppResult<Self> {
        let mut extra_bytes = Vec::with_capacity(items.len() + 1);
        let mut extra_lines = Vec::with_capacity(items.len() + 1);
        extra_bytes.push(0usize);
        extra_lines.push(0usize);
        let mut item_bytes = 0usize;
        let mut item_lines = 0usize;
        for (index, item) in items.iter().enumerate() {
            let serialized = if pretty {
                serde_json::to_string_pretty(item)
            } else {
                serde_json::to_string(item)
            }
            .map_err(json_measure_error)?;
            let lines = serialized.bytes().filter(|byte| *byte == b'\n').count() + 1;
            let embedded = if pretty {
                serialized
                    .len()
                    .saturating_add(item_indent.saturating_mul(lines))
            } else {
                serialized.len()
            };
            item_bytes = item_bytes.saturating_add(embedded);
            item_lines = item_lines.saturating_add(lines);
            let separators = if pretty { 2 * index } else { index };
            let array_layout = if pretty { item_indent } else { 0 };
            extra_bytes.push(
                item_bytes
                    .saturating_add(separators)
                    .saturating_add(array_layout),
            );
            extra_lines.push(if pretty {
                item_lines.saturating_add(1)
            } else {
                0
            });
        }
        Ok(Self {
            extra_bytes,
            extra_lines,
        })
    }

    pub(super) fn extra_bytes(&self, count: usize) -> usize {
        self.extra_bytes[count]
    }

    pub(super) fn extra_lines(&self, count: usize) -> usize {
        self.extra_lines[count]
    }
}

pub(super) struct JsonFragment {
    bytes: usize,
    lines: usize,
}

impl JsonFragment {
    pub(super) fn new<T: Serialize>(value: &T, pretty: bool) -> AppResult<Self> {
        let serialized = if pretty {
            serde_json::to_string_pretty(value)
        } else {
            serde_json::to_string(value)
        }
        .map_err(json_measure_error)?;
        Ok(Self {
            bytes: serialized.len(),
            lines: serialized.bytes().filter(|byte| *byte == b'\n').count() + 1,
        })
    }

    pub(super) fn with_array_prefix(mut self, prefix: &JsonArrayPrefix, count: usize) -> Self {
        self.bytes = self.bytes.saturating_add(prefix.extra_bytes(count));
        self.lines = self.lines.saturating_add(prefix.extra_lines(count));
        self
    }

    pub(super) fn embedded_bytes(&self, pretty: bool, indent: usize) -> usize {
        if pretty {
            self.bytes.saturating_add(indent.saturating_mul(self.lines))
        } else {
            self.bytes
        }
    }
}

fn json_measure_error(error: serde_json::Error) -> AppError {
    AppError::new(
        ErrorCode::IoError,
        format!("failed to measure JSON item output: {error}"),
        "Retry the command; if this repeats, report the command and `post --version`.",
    )
}

/// Counting variant for serializers that can measure without allocating the
/// complete body-bearing string on every candidate. Only the chosen prefix is
/// rendered, avoiding quadratic full-body copying for unlimited selections.
pub(super) fn admit_prefix_measured(
    selected_count: usize,
    max_bytes: usize,
    mut measure: impl FnMut(usize) -> AppResult<usize>,
    mut render: impl FnMut(usize) -> AppResult<String>,
) -> AppResult<PrefixAdmission> {
    if measure(selected_count)? <= max_bytes {
        let rendered = checked_render(render(selected_count)?, max_bytes)?;
        return Ok(PrefixAdmission {
            count: selected_count,
            rendered,
        });
    }
    let minimum = measure(0)?;
    if minimum > max_bytes {
        return Err(scaffold_too_large(max_bytes, minimum));
    }
    let mut admitted_count = 0;
    for count in 1..selected_count {
        if measure(count)? > max_bytes {
            break;
        }
        admitted_count = count;
    }
    let rendered = checked_render(render(admitted_count)?, max_bytes)?;
    Ok(PrefixAdmission {
        count: admitted_count,
        rendered,
    })
}

/// Admit a bounded read that reports skipped files, at the fullest report that
/// costs the reader no message. `attempt` runs an admission with the report at
/// the given detail. The listed report (a few ids and reasons) is preferred;
/// when it admits fewer messages than the count-only report, or does not fit at
/// all, the count-only report wins: corrupt files never push a readable
/// message out of a tight budget, they only shrink their own line.
pub(super) fn admit_with_skipped_detail(
    has_skipped: bool,
    selected_count: usize,
    mut attempt: impl FnMut(SkippedDetail) -> AppResult<PrefixAdmission>,
) -> AppResult<PrefixAdmission> {
    let listed = attempt(SkippedDetail::Listed);
    if !has_skipped || matches!(&listed, Ok(admission) if admission.count >= selected_count) {
        return listed;
    }
    match (listed, attempt(SkippedDetail::CountOnly)) {
        (Ok(listed), Ok(counted)) => Ok(if listed.count >= counted.count {
            listed
        } else {
            counted
        }),
        (Ok(listed), Err(_)) => Ok(listed),
        (Err(_), counted) => counted,
    }
}

pub(super) fn checked_render(rendered: String, max_bytes: usize) -> AppResult<String> {
    if rendered.len() <= max_bytes {
        return Ok(rendered);
    }
    Err(AppError::new(
        ErrorCode::IoError,
        format!(
            "bounded-output measurement drifted: rendered {} bytes for --max-bytes {max_bytes}",
            rendered.len()
        ),
        "Retry once; if this repeats, report the command and `post --version`.",
    )
    .input("--max-bytes")
    .reason("final render exceeded its preflight byte measurement"))
}

/// Find a continuation cap that fits its own rendered decimal budget fields.
/// Rendering the candidate again after the digit width grows makes this a
/// bounded fixed-point calculation rather than a guessed safety floor.
pub(super) fn minimum_progress_budget(
    initial: usize,
    mut measure_worst_progress: impl FnMut(usize) -> AppResult<usize>,
) -> AppResult<usize> {
    let mut budget = initial.max(1);
    for _ in 0..=(usize::BITS as usize + 1) {
        let required = measure_worst_progress(budget)?;
        if required <= budget {
            return Ok(budget);
        }
        budget = required;
    }
    Err(AppError::new(
        ErrorCode::IoError,
        "could not stabilize the measured continuation byte budget",
        "Retry the read; if this repeats, report the command and `post --version`.",
    )
    .reason("continuation budget digit width did not reach a bounded fixed point"))
}

pub(super) fn scaffold_too_large(max_bytes: usize, minimum_bytes: usize) -> AppError {
    // The requested limit is itself rendered in byte_limit and continuation
    // text. Crossing a decimal-width boundary can add a few bytes, so provide
    // a small safe retry margin instead of a self-invalidating exact fix.
    let safe_retry = minimum_bytes.saturating_add(64);
    AppError::new(
        ErrorCode::InvalidArgument,
        format!(
            "--max-bytes {max_bytes} is too small for the required bounded-output scaffold; it requires at least {minimum_bytes} bytes at this value width"
        ),
        format!("Retry with `--max-bytes {safe_retry}` or a larger value."),
    )
    .input("--max-bytes")
    .reason("required bounded-output scaffold exceeds the requested byte limit")
}

pub(super) struct SliceRequest {
    pub(super) start: usize,
    pub(super) source_end: usize,
    pub(super) total: usize,
}

pub(super) fn validate_slice_request(
    body: &str,
    offset: usize,
    length: Option<usize>,
) -> AppResult<SliceRequest> {
    let total = body.len();
    if offset > total {
        return Err(AppError::invalid_argument(format!(
            "--offset {offset} exceeds the body length of {total} UTF-8 bytes"
        )));
    }
    if !body.is_char_boundary(offset) {
        return Err(AppError::invalid_argument(format!(
            "--offset {offset} is not a UTF-8 code-point boundary"
        )));
    }
    let requested_end = match length {
        Some(length) => offset.checked_add(length).ok_or_else(|| {
            AppError::invalid_argument(format!(
                "--offset {offset} plus --length {length} overflows the platform byte range"
            ))
        })?,
        None => total,
    }
    .min(total);
    let mut source_end = requested_end;
    while source_end > offset && !body.is_char_boundary(source_end) {
        source_end -= 1;
    }
    if source_end == offset && offset < total {
        let next_width = body[offset..]
            .chars()
            .next()
            .expect("offset below total has a scalar")
            .len_utf8();
        return Err(AppError::new(
            ErrorCode::InvalidArgument,
            format!(
                "the requested source range contains no complete UTF-8 scalar; --length must be at least {next_width} byte(s) at offset {offset}"
            ),
            format!("Retry with `--offset {offset} --length {next_width}` or a larger length."),
        )
        .input("--length")
        .reason("source byte range ends inside the next UTF-8 scalar"));
    }
    Ok(SliceRequest {
        start: offset,
        source_end,
        total,
    })
}

/// Select the greatest code-point boundary in the requested source range
/// whose final encoded output fits. `content_cost` is additive for each
/// scalar; `scaffold_bytes` measures the same output with an empty body slice.
pub(super) fn select_slice_end(
    body: &str,
    request: &SliceRequest,
    max_bytes: usize,
    mut content_cost: impl FnMut(char) -> usize,
    mut scaffold_bytes: impl FnMut(usize) -> AppResult<usize>,
) -> AppResult<usize> {
    if request.start == request.total {
        let minimum = scaffold_bytes(request.start)?;
        if minimum > max_bytes {
            return Err(scaffold_too_large(max_bytes, minimum));
        }
        return Ok(request.start);
    }

    let mut encoded_body_bytes = 0usize;
    let mut best = None;
    let mut first_required = None;
    for (relative, scalar) in body[request.start..request.source_end].char_indices() {
        encoded_body_bytes = encoded_body_bytes.saturating_add(content_cost(scalar));
        let end = request.start + relative + scalar.len_utf8();
        let required = scaffold_bytes(end)?.saturating_add(encoded_body_bytes);
        first_required.get_or_insert(required);
        if required <= max_bytes {
            best = Some(end);
        }
    }
    best.ok_or_else(|| {
        let minimum = first_required.expect("non-empty scalar range has a first candidate");
        let safe_retry = minimum.saturating_add(64);
        AppError::new(
            ErrorCode::InvalidArgument,
            format!(
                "--max-bytes {max_bytes} is too small for the required slice scaffold plus the next complete UTF-8 scalar; it requires at least {minimum} bytes at this value width"
            ),
            format!("Retry with `--max-bytes {safe_retry}` or a larger value."),
        )
        .input("--max-bytes")
        .reason("the next complete UTF-8 scalar cannot fit with required slice metadata")
    })
}

pub(super) fn json_scalar_content_bytes(scalar: char) -> usize {
    match scalar {
        '"' | '\\' | '\u{0008}' | '\t' | '\n' | '\u{000c}' | '\r' => 2,
        '\u{0000}'..='\u{001f}' => 6,
        scalar => scalar.len_utf8(),
    }
}

pub(super) fn worst_json_scalar_content_bytes(body: &str) -> usize {
    body.chars()
        .map(json_scalar_content_bytes)
        .max()
        .unwrap_or(0)
}

/// Representative ranges whose decimal widths dominate every continuation
/// row for an unchanged body: widest nonterminal offsets, later EOF, and a
/// full-body EOF. Renderers may use empty body_slice and add the worst scalar
/// encoding cost separately.
pub(super) fn continuation_probe_ranges(total: usize) -> [(SliceRequest, usize); 3] {
    let nonterminal = total.saturating_sub(1);
    [
        (
            SliceRequest {
                start: nonterminal,
                source_end: nonterminal,
                total,
            },
            nonterminal,
        ),
        (
            SliceRequest {
                start: total,
                source_end: total,
                total,
            },
            total,
        ),
        (
            SliceRequest {
                start: 0,
                source_end: total,
                total,
            },
            total,
        ),
    ]
}

pub(super) fn gutter_scalar_content_bytes(scalar: char) -> usize {
    if scalar == '\n' {
        1 + crate::output::BODY_GUTTER.len()
    } else if scalar.is_control() && scalar != '\t' {
        0
    } else {
        scalar.len_utf8()
    }
}

#[cfg(test)]
mod tests {
    use super::{JsonArrayPrefix, JsonFragment};
    use serde::ser::Serializer;
    use serde::Serialize;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Serialize)]
    struct Top<'a, T> {
        messages: &'a [T],
    }

    #[derive(Serialize)]
    struct NestedTarget<'a, T> {
        messages: &'a [T],
    }

    #[derive(Serialize)]
    struct Nested<'a, T> {
        targets: Vec<NestedTarget<'a, T>>,
    }

    #[test]
    fn json_array_prefix_matches_real_compact_and_pretty_layouts() {
        let items = vec![
            serde_json::json!({"body": "quote=\" slash=\\ é🙂"}),
            serde_json::json!({"body": "line one\nline two"}),
        ];
        for pretty in [false, true] {
            let top_sizes = JsonArrayPrefix::new(&items, pretty, 4).expect("top sizes");
            let top_empty =
                crate::output::json_len(&Top::<serde_json::Value> { messages: &[] }, pretty)
                    .expect("top empty");
            let top_full =
                crate::output::json_len(&Top { messages: &items }, pretty).expect("top full");
            assert_eq!(top_empty + top_sizes.extra_bytes(items.len()), top_full);

            let nested_sizes = JsonArrayPrefix::new(&items, pretty, 8).expect("nested sizes");
            let nested_empty = crate::output::json_len(
                &Nested {
                    targets: vec![NestedTarget::<serde_json::Value> { messages: &[] }],
                },
                pretty,
            )
            .expect("nested empty");
            let nested_full = crate::output::json_len(
                &Nested {
                    targets: vec![NestedTarget { messages: &items }],
                },
                pretty,
            )
            .expect("nested full");
            assert_eq!(
                nested_empty + nested_sizes.extra_bytes(items.len()),
                nested_full
            );

            let outer_empty = crate::output::json_len(
                &Nested::<serde_json::Value> {
                    targets: Vec::new(),
                },
                pretty,
            )
            .expect("outer empty");
            let relative_sizes =
                JsonArrayPrefix::new(&items, pretty, 4).expect("relative nested sizes");
            let target =
                JsonFragment::new(&NestedTarget::<serde_json::Value> { messages: &[] }, pretty)
                    .expect("empty target")
                    .with_array_prefix(&relative_sizes, items.len());
            let target_array_layout = if pretty { 4 } else { 0 };
            assert_eq!(
                outer_empty + target.embedded_bytes(pretty, 4) + target_array_layout,
                nested_full,
                "relative message indentation plus outer target indentation must compose exactly"
            );
        }
    }

    struct Counted<'a>(&'a AtomicUsize);

    impl Serialize for Counted<'_> {
        fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
        where
            S: Serializer,
        {
            self.0.fetch_add(1, Ordering::Relaxed);
            serializer.serialize_str("payload")
        }
    }

    #[test]
    fn json_array_prefix_serializes_each_item_once_then_probes_in_constant_work() {
        let calls = AtomicUsize::new(0);
        let items = [Counted(&calls), Counted(&calls), Counted(&calls)];
        let sizes = JsonArrayPrefix::new(&items, false, 4).expect("prefix sizes");
        assert_eq!(calls.load(Ordering::Relaxed), items.len());
        for _ in 0..100 {
            for count in 0..=items.len() {
                let _ = sizes.extra_bytes(count);
            }
        }
        assert_eq!(
            calls.load(Ordering::Relaxed),
            items.len(),
            "prefix probes must not reserialize any item"
        );
    }
}
