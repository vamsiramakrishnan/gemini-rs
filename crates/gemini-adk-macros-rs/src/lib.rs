#![cfg_attr(docsrs, feature(doc_cfg))]
#![forbid(unsafe_code)]
//! Procedural macros for `gemini-adk-rs`.
//!
//! This crate provides the [`macro@tool`] attribute macro, which turns a plain
//! `async fn` into a registrable Gemini tool — eliminating the
//! `TypedTool::new::<Args>` + separate-args-struct ceremony.
//!
//! You normally don't depend on this crate directly. The [`macro@tool`] macro is
//! re-exported from `gemini-adk-rs` and the `gemini-adk-fluent-rs` prelude:
//!
//! ```ignore
//! use gemini_adk_fluent_rs::prelude::*;   // brings `tool` into scope
//! use serde_json::{json, Value};
//!
//! /// Get the current weather for a city.
//! ///
//! /// # Arguments
//! ///
//! /// * `city` - The city name.
//! /// * `units` - "metric" or "imperial"; metric when omitted.
//! #[tool]
//! async fn get_weather(city: String, units: Option<String>) -> Result<Value, ToolError> {
//!     Ok(json!({ "city": city, "units": units.unwrap_or("metric".into()) }))
//! }
//!
//! // `get_weather()` returns a value implementing `ToolFunction`.
//! let mut d = ToolDispatcher::new();
//! d.register_function(std::sync::Arc::new(get_weather()));
//! ```

use proc_macro::TokenStream;
use proc_macro2::Span;
use quote::{format_ident, quote};
use syn::{
    Data, DeriveInput, Expr, ExprLit, Fields, FnArg, ItemFn, Lit, LitInt, LitStr, Meta, Pat,
    PatType, ReturnType, Type, TypePath, parse_macro_input,
};

/// Where the runtime crate (`gemini-adk-rs`) is reachable from the expansion
/// site, as a path and as the string form the derive `crate = ".."` attributes
/// want.
///
/// A direct dependency wins (under whatever name it was renamed to); a crate
/// that depends only on `gemini-adk-fluent-rs`, or only on the `gemini-adk`
/// facade, reaches it through that crate's `gemini_adk_rs` re-export. Inside
/// `gemini-adk-rs` itself the crate declares `extern crate self as
/// gemini_adk_rs`, so the plain name works there and in its own tests.
fn runtime() -> (proc_macro2::TokenStream, String) {
    use proc_macro_crate::{FoundCrate, crate_name};
    match crate_name("gemini-adk-rs") {
        Ok(FoundCrate::Name(name)) => {
            let ident = format_ident!("{name}");
            (quote! { ::#ident }, name)
        }
        Ok(FoundCrate::Itself) => (quote! { ::gemini_adk_rs }, "gemini_adk_rs".to_string()),
        // Otherwise through a crate that re-exports it: the `gemini-adk`
        // facade (first, so its own tests exercise this path), or the fluent
        // layer it wraps.
        Err(_) => ["gemini-adk", "gemini-adk-fluent-rs"]
            .into_iter()
            .find_map(|facade| match crate_name(facade) {
                Ok(FoundCrate::Name(name)) => Some(name),
                Ok(FoundCrate::Itself) => Some(facade.replace('-', "_")),
                Err(_) => None,
            })
            .map_or_else(
                || (quote! { ::gemini_adk_rs }, "gemini_adk_rs".to_string()),
                |name| {
                    let ident = format_ident!("{name}");
                    (
                        quote! { ::#ident::gemini_adk_rs },
                        format!("{name}::gemini_adk_rs"),
                    )
                },
            ),
    }
}

/// Turn a documented `async fn` into a registrable Gemini tool.
///
/// The function's doc comment is what the model reads: its opening prose is
/// the tool's description, and a `# Arguments` section describes each
/// parameter. The parameter types are the schema.
///
/// ```ignore
/// /// Get the current weather for a city.
/// ///
/// /// # Arguments
/// ///
/// /// * `city` - The city name, e.g. "Paris".
/// /// * `units` - "metric" or "imperial"; metric when omitted.
/// #[tool]
/// async fn get_weather(city: String, units: Option<String>) -> Result<Weather, reqwest::Error> {
///     fetch_weather(&city, units.as_deref()).await
/// }
///
/// agent.tool(get_weather());
/// ```
///
/// # Description
///
/// The doc comment's prose up to its first `#` heading, with wrapped lines
/// joined. `#[tool("...")]` replaces it when the text for the model should
/// differ from the text for readers. A tool with neither is a compile error:
/// the model chooses tools by their descriptions.
///
/// # Arguments
///
/// Each item of a `# Arguments` (or `# Args`, `# Parameters`) section — in the
/// rustdoc form ``* `name` - text`` or ``- `name`: text`` — becomes that
/// parameter's schema `description`. Naming a parameter the function does not
/// have is a compile error, so the documentation cannot drift from the
/// signature.
///
/// Every parameter type must be `serde::Deserialize + schemars::JsonSchema`
/// and owned (`String`, not `&str`): arguments are deserialized from the
/// model's JSON. `Option<T>` parameters are optional. The schema is produced
/// by `gemini_adk_rs::tool::wire_schema`, so nested types are inlined and
/// optional fields declare a single type, as the API requires.
///
/// # Return type
///
/// - A type spelled `Result<T, E>` (under any path: `anyhow::Result<T>`,
///   `io::Result<T>`) is fallible. `T` is any `serde::Serialize` type; `E` is
///   any error — a `ToolError` keeps its variant, anything else becomes
///   `ToolError::ExecutionFailed` with its message.
/// - Any other type is the tool's output, and the tool cannot fail.
/// - No return type sends `null`.
///
/// A result that is not a JSON object reaches the model as `{"output": ..}`.
/// A `Result` behind an alias with another name is not recognized; spell the
/// return type as `Result<..>`.
///
/// # What it generates
///
/// A constructor `fn get_weather() -> impl ToolFunction` (with the original
/// visibility and doc comment) whose value you register:
/// `agent.tool(get_weather())`, or
/// `dispatcher.register_function(Arc::new(get_weather()))`. The original body
/// runs in a hidden `async fn`, which keeps the function's other attributes
/// (`#[allow]`, `#[tracing::instrument]`, ...); `#[cfg]` applies to every
/// generated item.
///
/// # Path hygiene
///
/// Generated code reaches `serde`, `schemars`, `serde_json`, and `async_trait`
/// through the runtime crate's `__macros` module, so none of them need to be
/// in your `Cargo.toml`. The runtime crate is located at expansion time:
/// `gemini-adk-rs` if it is a direct dependency (under whatever name), else
/// through the re-export in `gemini-adk` or `gemini-adk-fluent-rs`.
#[proc_macro_attribute]
pub fn tool(attr: TokenStream, item: TokenStream) -> TokenStream {
    let description = if attr.is_empty() {
        None
    } else {
        Some(parse_macro_input!(attr as LitStr))
    };
    let func = parse_macro_input!(item as ItemFn);

    match expand(description, func) {
        Ok(ts) => ts.into(),
        Err(e) => e.to_compile_error().into(),
    }
}

/// What a `#[tool]` fn's doc comment says, split into the parts the model sees.
#[derive(Debug, Default, PartialEq)]
struct ToolDocs {
    /// Prose before the first heading, paragraphs separated by a blank line.
    description: String,
    /// `(parameter, description)` from the `# Arguments` section, in order.
    arguments: Vec<(String, String)>,
}

/// The text of each `#[doc = ".."]` attribute, one entry per doc line.
fn doc_lines(attrs: &[syn::Attribute]) -> Vec<String> {
    attrs
        .iter()
        .filter(|a| a.path().is_ident("doc"))
        .filter_map(|a| match &a.meta {
            Meta::NameValue(nv) => match &nv.value {
                Expr::Lit(ExprLit {
                    lit: Lit::Str(s), ..
                }) => Some(s.value()),
                _ => None,
            },
            _ => None,
        })
        .flat_map(|text| text.lines().map(str::to_owned).collect::<Vec<_>>())
        .collect()
}

/// Split doc lines into the description and the `# Arguments` items.
fn parse_docs(lines: &[String]) -> ToolDocs {
    let mut docs = ToolDocs::default();
    let mut paragraphs: Vec<String> = Vec::new();
    let mut paragraph = String::new();
    // `None` before the first heading; then whether we are in an arguments section.
    let mut section: Option<bool> = None;

    let flush = |paragraph: &mut String, paragraphs: &mut Vec<String>| {
        if !paragraph.is_empty() {
            paragraphs.push(std::mem::take(paragraph));
        }
    };

    let mut in_code = false;
    for raw in lines {
        let line = raw.trim();
        // Code blocks are for readers, and their `# hidden` lines are not headings.
        if line.starts_with("```") || line.starts_with("~~~") {
            in_code = !in_code;
            continue;
        }
        if in_code {
            continue;
        }
        if let Some(heading) = line.strip_prefix('#') {
            let heading = heading.trim_start_matches('#').trim().to_ascii_lowercase();
            section = Some(matches!(
                heading.as_str(),
                "arguments" | "args" | "parameters" | "params"
            ));
            continue;
        }
        match section {
            None if line.is_empty() => flush(&mut paragraph, &mut paragraphs),
            None => {
                if !paragraph.is_empty() {
                    paragraph.push(' ');
                }
                paragraph.push_str(line);
            }
            Some(true) => {
                if let Some(item) = line.strip_prefix(['*', '-']) {
                    if let Some(parsed) = parse_argument_item(item) {
                        docs.arguments.push(parsed);
                    }
                } else if !line.is_empty()
                    && let Some((_, text)) = docs.arguments.last_mut()
                {
                    if !text.is_empty() {
                        text.push(' ');
                    }
                    text.push_str(line);
                }
            }
            Some(false) => {}
        }
    }
    flush(&mut paragraph, &mut paragraphs);
    docs.description = paragraphs.join("\n\n");
    docs
}

/// Parse ``` `name` - text``` / ``name: text`` into `(name, text)`.
fn parse_argument_item(item: &str) -> Option<(String, String)> {
    let item = item.trim();
    let (name, rest) = if let Some(quoted) = item.strip_prefix('`') {
        let end = quoted.find('`')?;
        (&quoted[..end], &quoted[end + 1..])
    } else {
        let end = item
            .find(|c: char| !(c.is_alphanumeric() || c == '_'))
            .unwrap_or(item.len());
        (&item[..end], &item[end..])
    };
    if name.is_empty() {
        return None;
    }
    let text = rest
        .trim_start()
        .trim_start_matches(['-', ':', '\u{2013}', '\u{2014}'])
        .trim();
    Some((name.to_owned(), text.to_owned()))
}

/// Whether a return type is spelled `Result<..>` under any path.
/// Whether `ty` is the runtime's `ToolContext` (by its last path segment),
/// which `#[tool]` injects rather than asks the model for.
fn is_tool_context(ty: &Type) -> bool {
    match ty {
        Type::Path(path) => path
            .path
            .segments
            .last()
            .is_some_and(|seg| seg.ident == "ToolContext" && seg.arguments.is_empty()),
        _ => false,
    }
}

fn is_result(ty: &Type) -> bool {
    let Type::Path(TypePath { qself: None, path }) = ty else {
        return false;
    };
    path.segments
        .last()
        .is_some_and(|seg| seg.ident == "Result" && !seg.arguments.is_none())
}

fn expand(description: Option<LitStr>, func: ItemFn) -> syn::Result<proc_macro2::TokenStream> {
    let sig = &func.sig;

    if sig.asyncness.is_none() {
        return Err(syn::Error::new_spanned(
            sig.fn_token,
            "#[tool] requires an `async fn`",
        ));
    }
    if let Some(variadic) = &sig.variadic {
        return Err(syn::Error::new_spanned(
            variadic,
            "#[tool] does not support variadic functions",
        ));
    }
    if !sig.generics.params.is_empty() {
        return Err(syn::Error::new_spanned(
            sig.generics.clone(),
            "#[tool] does not support generic functions",
        ));
    }

    let fn_name = &sig.ident;
    let vis = &func.vis;
    let body = &func.block;

    // Collect (ident, type) for each parameter; reject `self` receivers,
    // patterns and borrowed types.
    let mut field_idents = Vec::new();
    let mut field_types = Vec::new();
    // Every parameter in declaration order (for the preserved body), and the
    // one typed `ToolContext`, which the runtime fills instead of the model.
    let mut param_idents = Vec::new();
    let mut param_types = Vec::new();
    let mut context_ident: Option<syn::Ident> = None;
    for input in &sig.inputs {
        match input {
            FnArg::Receiver(r) => {
                return Err(syn::Error::new_spanned(
                    r,
                    "#[tool] cannot be applied to methods taking `self`",
                ));
            }
            FnArg::Typed(PatType { pat, ty, .. }) => {
                let ident = match pat.as_ref() {
                    Pat::Ident(pat_ident) => pat_ident.ident.clone(),
                    other => {
                        return Err(syn::Error::new_spanned(
                            other,
                            "#[tool] parameters must be simple identifiers (no patterns)",
                        ));
                    }
                };
                param_idents.push(ident.clone());
                param_types.push((*ty).clone());
                if is_tool_context(ty) {
                    if context_ident.is_some() {
                        return Err(syn::Error::new_spanned(
                            ty,
                            "#[tool] takes at most one `ToolContext` parameter",
                        ));
                    }
                    context_ident = Some(ident);
                    continue;
                }
                if let Type::Reference(reference) = ty.as_ref() {
                    return Err(syn::Error::new_spanned(
                        reference,
                        "#[tool] parameters are deserialized from the model's JSON, so they \
                         must be owned: use `String` for `&str`, `Vec<T>` for `&[T]`",
                    ));
                }
                field_idents.push(ident);
                field_types.push((*ty).clone());
            }
        }
    }

    // What the model is told: the attribute wins, else the doc comment.
    let docs = parse_docs(&doc_lines(&func.attrs));
    for (name, _) in &docs.arguments {
        if !field_idents.iter().any(|ident| ident == name) {
            return Err(syn::Error::new_spanned(
                fn_name,
                format!(
                    "the `# Arguments` section documents `{name}`, which is not a parameter \
                     of `{fn_name}`"
                ),
            ));
        }
    }
    let description = match description {
        Some(text) => text.value(),
        None if !docs.description.is_empty() => docs.description.clone(),
        None => {
            return Err(syn::Error::new_spanned(
                fn_name,
                "#[tool] needs a description for the model: add a `///` doc comment to the \
                 function, or pass one as `#[tool(\"...\")]`",
            ));
        }
    };

    // The body keeps its declared return type; the tool adapts it.
    let (return_type, adapt) = match &sig.output {
        ReturnType::Default => (quote! { () }, quote! { tool_output }),
        ReturnType::Type(_, ty) if is_result(ty) => (quote! { #ty }, quote! { tool_result }),
        ReturnType::Type(_, ty) => (quote! { #ty }, quote! { tool_output }),
    };

    // `#[cfg]` gates every generated item; docs and `#[deprecated]` describe
    // the constructor users call; everything else belongs with the body.
    let mut cfg_attrs = Vec::new();
    let mut constructor_attrs = Vec::new();
    let mut body_attrs = Vec::new();
    for attr in &func.attrs {
        let path = attr.path();
        if path.is_ident("cfg") {
            cfg_attrs.push(attr);
        } else if path.is_ident("doc") || path.is_ident("deprecated") {
            constructor_attrs.push(attr);
        } else {
            body_attrs.push(attr);
        }
    }

    // Naming for generated items, derived from the (Pascal-cased) fn name.
    let pascal = to_pascal_case(&fn_name.to_string());
    let args_struct = format_ident!("__{}Args", pascal);
    let tool_struct = format_ident!("__{}Tool", pascal);
    // The inner async fn that holds the original body, invoked from `call`.
    let inner_fn = format_ident!("__{}_impl", fn_name);

    let fn_name_str = fn_name.to_string();

    // Build the hidden args struct fields; a documented parameter carries its
    // text as a doc comment, which the schema derive turns into `description`.
    let struct_fields = field_idents
        .iter()
        .zip(field_types.iter())
        .map(|(ident, ty)| {
            let doc = docs
                .arguments
                .iter()
                .find(|(name, _)| ident == name)
                .map(|(_, text)| text.as_str())
                .filter(|text| !text.is_empty())
                .map(|text| quote! { #[doc = #text] });
            // `Option<T>` fields default to `None` when absent from the JSON.
            // No trailing comma here — `#(#struct_fields),*` adds the separators.
            let default = is_option(ty).then(|| quote! { #[serde(default)] });
            quote! {
                #doc
                #default
                #ident: #ty
            }
        });

    // Upstream crates are reached through `gemini_adk_rs::__macros` so the consumer
    // doesn't need them in scope under those exact names.
    let (rt, rt_str) = runtime();
    let serde = quote! { #rt::__macros::serde };
    let schemars = quote! { #rt::__macros::schemars };
    let async_trait = quote! { #rt::__macros::async_trait };
    let serde_json = quote! { #rt::__macros::serde_json };
    let serde_crate = LitStr::new(&format!("{rt_str}::__macros::serde"), Span::call_site());
    let schemars_crate = LitStr::new(&format!("{rt_str}::__macros::schemars"), Span::call_site());

    // Bind the runtime's context to the parameter that asked for it.
    let bind_context = match &context_ident {
        Some(ident) => quote! { let #ident = ctx; },
        None => quote! { let _ = ctx; },
    };

    let expanded = quote! {
        // Hidden args struct: drives both deserialization and schema generation.
        #(#cfg_attrs)*
        #[derive(#serde::Deserialize, #schemars::JsonSchema)]
        #[serde(crate = #serde_crate)]
        #[schemars(crate = #schemars_crate)]
        #[allow(non_camel_case_types, non_snake_case)]
        struct #args_struct {
            #(#struct_fields),*
        }

        // The original function body, preserved verbatim as a free async fn.
        #(#cfg_attrs)*
        #(#body_attrs)*
        #[allow(non_snake_case)]
        async fn #inner_fn ( #(#param_idents : #param_types),* ) -> #return_type #body

        // Hidden tool type implementing `ToolFunction`.
        #(#cfg_attrs)*
        #[allow(non_camel_case_types)]
        #[derive(Clone, Copy, Debug, Default)]
        #vis struct #tool_struct;

        #(#cfg_attrs)*
        #[#async_trait::async_trait]
        impl #rt::tool::ToolFunction for #tool_struct {
            fn name(&self) -> &str {
                #fn_name_str
            }

            fn description(&self) -> &str {
                #description
            }

            fn parameters(&self) -> ::core::option::Option<#serde_json::Value> {
                // The args struct's name is an implementation detail; the
                // tool's name and description are what the model reads.
                let mut schema = #rt::tool::wire_schema::<#args_struct>();
                if let ::core::option::Option::Some(object) = schema.as_object_mut() {
                    object.remove("title");
                }
                ::core::option::Option::Some(schema)
            }

            async fn call(
                &self,
                args: #serde_json::Value,
            ) -> ::core::result::Result<#serde_json::Value, #rt::error::ToolError> {
                self.call_with_context(args, #rt::tool::ToolContext::detached()).await
            }

            async fn call_with_context(
                &self,
                args: #serde_json::Value,
                ctx: #rt::tool::ToolContext,
            ) -> ::core::result::Result<#serde_json::Value, #rt::error::ToolError> {
                let #args_struct { #(#field_idents),* } =
                    #serde_json::from_value(args).map_err(|e| {
                        #rt::error::ToolError::InvalidArgs(
                            ::std::format!("Failed to deserialize arguments: {e}"),
                        )
                    })?;
                #bind_context
                #rt::__macros::#adapt(#inner_fn ( #(#param_idents),* ).await)
            }
        }

        // Public constructor: `fn foo() -> __FooTool`.
        #(#cfg_attrs)*
        #(#constructor_attrs)*
        #[allow(non_snake_case)]
        #vis fn #fn_name () -> #tool_struct {
            #tool_struct
        }
    };

    Ok(expanded)
}

/// Derive an `Extract` record builder from a struct's fields.
///
/// Each field carries a `#[recognize(..)]` attribute naming a deterministic
/// recognizer; the macro generates an inherent `fn extract() -> Extract` that
/// builds the record. The field name becomes the record field name and (by
/// default) its `State` key.
///
/// ```ignore
/// use gemini_adk_rs::extract::Extract;   // the type — same name, type namespace
/// use gemini_adk_rs::Extract;            // the derive — macro namespace
///
/// #[derive(Extract)]
/// #[extract(name = "order", window = 3)]
/// struct Order {
///     #[recognize(integer_near = ["want", "get"])]
///     quantity: Option<i64>,
///     #[recognize(one_of = ["pizza", "salad", "soda"])]
///     item: Option<String>,
///     #[recognize(datetime)]
///     #[extract(state = "when")]
///     pickup: Option<serde_json::Value>,
///     #[recognize(yes_no)]
///     confirmed: Option<bool>,
/// }
///
/// let record: Extract = Order::extract();
/// ```
///
/// # Recognizer forms
///
/// | Attribute | Recognizer |
/// |---|---|
/// | `#[recognize(integer)]` | `Recognizer::integer()` |
/// | `#[recognize(integer_near = ["a", "b"])]` | `Recognizer::integer_near([..])` |
/// | `#[recognize(money)]` | `Recognizer::money()` |
/// | `#[recognize(regex = "pat")]` | `Recognizer::regex("pat")` |
/// | `#[recognize(one_of = ["a", "b"])]` | `Recognizer::one_of([..])` |
/// | `#[recognize(fuzzy = ["a", "b"])]` | `Recognizer::fuzzy([..])` |
/// | `#[recognize(yes_no)]` | `Recognizer::yes_no()` |
/// | `#[recognize(datetime)]` | `Recognizer::datetime()` |
///
/// # Options
///
/// - Container `#[extract(name = "...")]` — record name (default: the struct
///   name in `snake_case`).
/// - Container `#[extract(window = N)]` — transcript window (default `3`).
/// - Field `#[extract(state = "key")]` — promote to a custom `State` key.
///
/// Fields without a `#[recognize(..)]` attribute are ignored.
#[proc_macro_derive(Extract, attributes(recognize, extract))]
pub fn derive_extract(item: TokenStream) -> TokenStream {
    let input = parse_macro_input!(item as DeriveInput);
    match expand_extract(input) {
        Ok(ts) => ts.into(),
        Err(e) => e.to_compile_error().into(),
    }
}

fn expand_extract(input: DeriveInput) -> syn::Result<proc_macro2::TokenStream> {
    let (rt, _) = runtime();
    let ident = &input.ident;

    let fields = match &input.data {
        Data::Struct(s) => match &s.fields {
            Fields::Named(named) => &named.named,
            _ => {
                return Err(syn::Error::new_spanned(
                    ident,
                    "#[derive(Extract)] requires a struct with named fields",
                ));
            }
        },
        _ => {
            return Err(syn::Error::new_spanned(
                ident,
                "#[derive(Extract)] can only be applied to structs",
            ));
        }
    };

    // Container options: name + window.
    let mut name = to_snake_case(&ident.to_string());
    let mut window: usize = 3;
    for attr in &input.attrs {
        if attr.path().is_ident("extract") {
            attr.parse_nested_meta(|meta| {
                if meta.path.is_ident("name") {
                    let v: LitStr = meta.value()?.parse()?;
                    name = v.value();
                } else if meta.path.is_ident("window") {
                    let v: LitInt = meta.value()?.parse()?;
                    window = v.base10_parse()?;
                } else {
                    return Err(
                        meta.error("unknown `extract` option (expected `name` or `window`)")
                    );
                }
                Ok(())
            })?;
        }
    }

    // Every named field, referenced by a hidden marker method so that deriving
    // `Extract` on an otherwise-unread struct does not trip `dead_code`.
    let all_field_idents: Vec<_> = fields.iter().filter_map(|f| f.ident.clone()).collect();

    // One `.field(..)` / `.field_to(..)` call per recognized field.
    let mut field_calls = Vec::new();
    for field in fields {
        let Some(recognize) = field.attrs.iter().find(|a| a.path().is_ident("recognize")) else {
            continue;
        };
        let fname = field.ident.as_ref().expect("named field").to_string();
        let recognizer = recognizer_expr(recognize)?;

        // Optional per-field state-key override.
        let mut state_key: Option<String> = None;
        for attr in &field.attrs {
            if attr.path().is_ident("extract") {
                attr.parse_nested_meta(|meta| {
                    if meta.path.is_ident("state") {
                        let v: LitStr = meta.value()?.parse()?;
                        state_key = Some(v.value());
                    } else {
                        return Err(meta.error("unknown field `extract` option (expected `state`)"));
                    }
                    Ok(())
                })?;
            }
        }

        field_calls.push(match state_key {
            Some(sk) => quote! { .field_to(#fname, #sk, #recognizer) },
            None => quote! { .field(#fname, #recognizer) },
        });
    }

    let doc = format!("The `Extract` record derived from `{ident}`'s `#[recognize(..)]` fields.");
    Ok(quote! {
        impl #ident {
            #[doc = #doc]
            pub fn extract() -> #rt::extract::Extract {
                #rt::extract::Extract::record(#name)
                    #(#field_calls)*
                    .window(#window)
                    .build()
            }

            #[allow(dead_code)]
            #[doc(hidden)]
            fn __extract_mark_fields_used(&self) {
                #( let _ = &self.#all_field_idents; )*
            }
        }
    })
}

/// Derive a [`Frame`] impl from a struct's `#[slot(..)]` fields.
///
/// Every named field becomes a slot (state key = field name unless overridden).
/// The generated `fn frame() -> FrameSpec` carries each slot's prompt, reprompt,
/// confirmation policy, and PII flag — the metadata the conversation compiler and
/// repair use.
///
/// ```ignore
/// #[derive(Frame)]
/// #[frame(name = "booking")]
/// struct Booking {
///     #[slot(prompt = "For how many people?", confirm = "low_confidence")]
///     party_size: u8,
///     #[slot(prompt = "Name?", pii)]
///     name: String,
/// }
/// ```
///
/// Field `#[slot(..)]` options: `prompt`, `reprompt`, `confirm`
/// (`never`/`low_confidence`/`always`), `state` (key override), `pii` (flag).
/// Container `#[frame(name = "...")]` sets the frame name.
#[proc_macro_derive(Frame, attributes(slot, frame, recognize))]
pub fn derive_frame(item: TokenStream) -> TokenStream {
    let input = parse_macro_input!(item as DeriveInput);
    match expand_frame(input) {
        Ok(ts) => ts.into(),
        Err(e) => e.to_compile_error().into(),
    }
}

fn expand_frame(input: DeriveInput) -> syn::Result<proc_macro2::TokenStream> {
    let (rt, _) = runtime();
    let ident = &input.ident;

    let fields = match &input.data {
        Data::Struct(s) => match &s.fields {
            Fields::Named(named) => &named.named,
            _ => {
                return Err(syn::Error::new_spanned(
                    ident,
                    "#[derive(Frame)] requires a struct with named fields",
                ));
            }
        },
        _ => {
            return Err(syn::Error::new_spanned(
                ident,
                "#[derive(Frame)] can only be applied to structs",
            ));
        }
    };

    // Container `#[frame(name = "...")]`.
    let mut name = to_snake_case(&ident.to_string());
    for attr in &input.attrs {
        if attr.path().is_ident("frame") {
            attr.parse_nested_meta(|meta| {
                if meta.path.is_ident("name") {
                    let v: LitStr = meta.value()?.parse()?;
                    name = v.value();
                    Ok(())
                } else {
                    Err(meta.error("unknown `frame` option (expected `name`)"))
                }
            })?;
        }
    }

    let all_field_idents: Vec<_> = fields.iter().filter_map(|f| f.ident.clone()).collect();

    let mut slot_exprs = Vec::new();
    for field in fields {
        let fname = field.ident.as_ref().expect("named field").to_string();
        let mut state_key = fname.clone();
        let mut prompt: Option<String> = None;
        let mut reprompt: Option<String> = None;
        let mut confirm = quote! { #rt::frame::ConfirmPolicy::Never };
        let mut pii = false;
        let mut min: Option<f64> = None;
        let mut max: Option<f64> = None;
        let mut non_empty = false;

        // Optional `#[recognize(..)]` (same vocabulary as `#[derive(Extract)]`).
        let recognizer = match field.attrs.iter().find(|a| a.path().is_ident("recognize")) {
            Some(attr) => {
                let r = slot_recognizer_expr(attr)?;
                quote! { Some(#r) }
            }
            None => quote! { None },
        };

        for attr in &field.attrs {
            if !attr.path().is_ident("slot") {
                continue;
            }
            attr.parse_nested_meta(|meta| {
                if meta.path.is_ident("prompt") {
                    let v: LitStr = meta.value()?.parse()?;
                    prompt = Some(v.value());
                } else if meta.path.is_ident("reprompt") {
                    let v: LitStr = meta.value()?.parse()?;
                    reprompt = Some(v.value());
                } else if meta.path.is_ident("state") {
                    let v: LitStr = meta.value()?.parse()?;
                    state_key = v.value();
                } else if meta.path.is_ident("confirm") {
                    let v: LitStr = meta.value()?.parse()?;
                    confirm = match v.value().as_str() {
                        "never" => quote! { #rt::frame::ConfirmPolicy::Never },
                        "low_confidence" => {
                            quote! { #rt::frame::ConfirmPolicy::LowConfidence }
                        }
                        "always" => quote! { #rt::frame::ConfirmPolicy::Always },
                        other => {
                            return Err(meta.error(format!(
                                "unknown confirm policy '{other}' (expected never/low_confidence/always)"
                            )))
                        }
                    };
                } else if meta.path.is_ident("pii") {
                    pii = true;
                } else if meta.path.is_ident("min") {
                    min = Some(lit_to_f64(&meta.value()?.parse()?)?);
                } else if meta.path.is_ident("max") {
                    max = Some(lit_to_f64(&meta.value()?.parse()?)?);
                } else if meta.path.is_ident("non_empty") {
                    non_empty = true;
                } else {
                    return Err(meta.error(
                        "unknown `slot` option (expected prompt/reprompt/state/confirm/pii/min/max/non_empty)",
                    ));
                }
                Ok(())
            })?;
        }

        // Lower min/max/non_empty into a serializable SlotValidator.
        let validate = if min.is_some() || max.is_some() {
            let min_tok = match min {
                Some(v) => quote! { Some(#v) },
                None => quote! { None },
            };
            let max_tok = match max {
                Some(v) => quote! { Some(#v) },
                None => quote! { None },
            };
            quote! { Some(#rt::frame::SlotValidator::Range { min: #min_tok, max: #max_tok }) }
        } else if non_empty {
            quote! { Some(#rt::frame::SlotValidator::NonEmpty) }
        } else {
            quote! { None }
        };

        let prompt_tok = match prompt {
            Some(p) => quote! { Some(#p.to_string()) },
            None => quote! { None },
        };
        let reprompt_tok = match reprompt {
            Some(p) => quote! { Some(#p.to_string()) },
            None => quote! { None },
        };
        slot_exprs.push(quote! {
            #rt::frame::SlotSpec {
                name: #fname.to_string(),
                state_key: #state_key.to_string(),
                prompt: #prompt_tok,
                reprompt: #reprompt_tok,
                confirm: #confirm,
                pii: #pii,
                recognizer: #recognizer,
                validate: #validate,
            }
        });
    }

    let doc = format!("The `FrameSpec` derived from `{ident}`'s `#[slot(..)]` fields.");
    Ok(quote! {
        impl #rt::frame::Frame for #ident {
            #[doc = #doc]
            fn frame() -> #rt::frame::FrameSpec {
                #rt::frame::FrameSpec {
                    name: #name.to_string(),
                    slots: ::std::vec![ #(#slot_exprs),* ],
                }
            }
        }

        impl #ident {
            #[allow(dead_code)]
            #[doc(hidden)]
            fn __frame_mark_fields_used(&self) {
                #( let _ = &self.#all_field_idents; )*
            }
        }
    })
}

/// Build the `Recognizer::..` expression for a single `#[recognize(..)]` attr.
fn recognizer_expr(attr: &syn::Attribute) -> syn::Result<proc_macro2::TokenStream> {
    let (rt, _) = runtime();
    let r = quote! { #rt::extract::Recognizer };
    let meta: Meta = attr.parse_args()?;
    match meta {
        Meta::Path(p) => {
            let id = p
                .get_ident()
                .ok_or_else(|| syn::Error::new_spanned(&p, "expected a recognizer name"))?;
            match id.to_string().as_str() {
                "integer" => Ok(quote! { #r::integer() }),
                "money" => Ok(quote! { #r::money() }),
                "yes_no" => Ok(quote! { #r::yes_no() }),
                "datetime" => Ok(quote! { #r::datetime() }),
                other => Err(syn::Error::new_spanned(
                    &p,
                    format!("unknown recognizer `{other}`"),
                )),
            }
        }
        Meta::NameValue(nv) => {
            let id = nv
                .path
                .get_ident()
                .ok_or_else(|| syn::Error::new_spanned(&nv.path, "expected a recognizer name"))?;
            match id.to_string().as_str() {
                "integer_near" => {
                    let a = str_array(&nv.value)?;
                    Ok(quote! { #r::integer_near([ #(#a),* ]) })
                }
                "one_of" => {
                    let a = str_array(&nv.value)?;
                    Ok(quote! { #r::one_of([ #(#a),* ]) })
                }
                "fuzzy" => {
                    let a = str_array(&nv.value)?;
                    Ok(quote! { #r::fuzzy([ #(#a),* ]) })
                }
                "regex" => {
                    let s = str_lit(&nv.value)?;
                    Ok(quote! { #r::regex(#s) })
                }
                other => Err(syn::Error::new_spanned(
                    &nv.path,
                    format!("`{other}` does not take a value"),
                )),
            }
        }
        Meta::List(l) => Err(syn::Error::new_spanned(
            l,
            "unexpected nested list in `#[recognize(..)]`",
        )),
    }
}

/// Build a serializable `SlotRecognizer` expression for a `#[recognize(..)]` attr
/// on a `#[derive(Frame)]` field (same vocabulary as the Extract derive).
fn slot_recognizer_expr(attr: &syn::Attribute) -> syn::Result<proc_macro2::TokenStream> {
    let (rt, _) = runtime();
    let r = quote! { #rt::frame::SlotRecognizer };
    let meta: Meta = attr.parse_args()?;
    match meta {
        Meta::Path(p) => {
            let id = p
                .get_ident()
                .ok_or_else(|| syn::Error::new_spanned(&p, "expected a recognizer name"))?;
            match id.to_string().as_str() {
                "integer" => Ok(quote! { #r::Integer }),
                "money" => Ok(quote! { #r::Money }),
                "yes_no" => Ok(quote! { #r::YesNo }),
                "datetime" => Ok(quote! { #r::DateTime }),
                other => Err(syn::Error::new_spanned(
                    &p,
                    format!("unknown recognizer `{other}`"),
                )),
            }
        }
        Meta::NameValue(nv) => {
            let id = nv
                .path
                .get_ident()
                .ok_or_else(|| syn::Error::new_spanned(&nv.path, "expected a recognizer name"))?;
            match id.to_string().as_str() {
                "integer_near" => {
                    let a = str_array(&nv.value)?;
                    Ok(quote! { #r::IntegerNear(::std::vec![ #(#a.to_string()),* ]) })
                }
                "one_of" => {
                    let a = str_array(&nv.value)?;
                    Ok(quote! { #r::OneOf(::std::vec![ #(#a.to_string()),* ]) })
                }
                "fuzzy" => {
                    let a = str_array(&nv.value)?;
                    Ok(quote! { #r::Fuzzy(::std::vec![ #(#a.to_string()),* ]) })
                }
                "regex" => {
                    let s = str_lit(&nv.value)?;
                    Ok(quote! { #r::Regex(#s.to_string()) })
                }
                other => Err(syn::Error::new_spanned(
                    &nv.path,
                    format!("`{other}` does not take a value"),
                )),
            }
        }
        Meta::List(l) => Err(syn::Error::new_spanned(
            l,
            "unexpected nested list in `#[recognize(..)]`",
        )),
    }
}

/// Parse an integer or float literal into an `f64` (for slot `min`/`max`).
fn lit_to_f64(lit: &Lit) -> syn::Result<f64> {
    match lit {
        Lit::Int(i) => i.base10_parse::<f64>(),
        Lit::Float(f) => f.base10_parse::<f64>(),
        other => Err(syn::Error::new_spanned(
            other,
            "expected a numeric literal for `min`/`max`",
        )),
    }
}

/// Parse an expression that must be an array of string literals.
fn str_array(expr: &Expr) -> syn::Result<Vec<LitStr>> {
    match expr {
        Expr::Array(arr) => arr
            .elems
            .iter()
            .map(|e| match e {
                Expr::Lit(ExprLit {
                    lit: Lit::Str(s), ..
                }) => Ok(s.clone()),
                other => Err(syn::Error::new_spanned(
                    other,
                    "expected a string literal in the array",
                )),
            })
            .collect(),
        other => Err(syn::Error::new_spanned(
            other,
            "expected an array of string literals, e.g. [\"a\", \"b\"]",
        )),
    }
}

/// Parse an expression that must be a single string literal.
fn str_lit(expr: &Expr) -> syn::Result<LitStr> {
    match expr {
        Expr::Lit(ExprLit {
            lit: Lit::Str(s), ..
        }) => Ok(s.clone()),
        other => Err(syn::Error::new_spanned(other, "expected a string literal")),
    }
}

/// Convert a `PascalCase`/`camelCase` identifier to `snake_case`.
fn to_snake_case(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 4);
    for (i, ch) in s.chars().enumerate() {
        if ch.is_uppercase() {
            if i != 0 {
                out.push('_');
            }
            out.extend(ch.to_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
}

/// Returns `true` if `ty` is syntactically an `Option<...>`.
///
/// Accepts the prelude name (`Option`) and the spelled-out std/core paths
/// (`option::Option`, `std::option::Option`, `core::option::Option`, with or
/// without a leading `::`). The full path is checked — a user type like
/// `my::Option` does NOT match. Purely syntactic: a type alias or renamed
/// import of `Option` is invisible to the macro, as with any derive.
fn is_option(ty: &Type) -> bool {
    let Type::Path(TypePath { qself: None, path }) = ty else {
        return false;
    };
    // Only the final `Option` segment may carry generic arguments.
    if path
        .segments
        .iter()
        .rev()
        .skip(1)
        .any(|seg| !seg.arguments.is_none())
    {
        return false;
    }
    let idents: Vec<&syn::Ident> = path.segments.iter().map(|seg| &seg.ident).collect();
    match idents.as_slice() {
        // `Option<T>` / `option::Option<T>` resolve via the prelude only when
        // the path is relative.
        [opt] => path.leading_colon.is_none() && *opt == "Option",
        [module, opt] => path.leading_colon.is_none() && *module == "option" && *opt == "Option",
        // `std::option::Option<T>` / `core::option::Option<T>`, `::`-rooted or not.
        [root, module, opt] => {
            (*root == "std" || *root == "core") && *module == "option" && *opt == "Option"
        }
        _ => false,
    }
}

/// Convert a `snake_case` identifier to `PascalCase`.
fn to_pascal_case(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut upper_next = true;
    for ch in s.chars() {
        if ch == '_' {
            upper_next = true;
        } else if upper_next {
            out.extend(ch.to_uppercase());
            upper_next = false;
        } else {
            out.push(ch);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{ToolDocs, is_option, is_result, parse_docs};
    use syn::parse_quote;

    fn docs(text: &str) -> ToolDocs {
        let lines: Vec<String> = text.lines().map(|l| format!(" {l}")).collect();
        parse_docs(&lines)
    }

    #[test]
    fn description_is_the_prose_before_the_first_heading() {
        let parsed =
            docs("Get the weather\nfor a city.\n\nUses the cached forecast.\n\n# Errors\n\nNever.");
        assert_eq!(
            parsed.description,
            "Get the weather for a city.\n\nUses the cached forecast."
        );
        assert!(parsed.arguments.is_empty());
    }

    #[test]
    fn arguments_accept_the_common_rustdoc_forms() {
        let parsed = docs(
            "Look up.\n\n# Arguments\n\n\
             * `city` - The city,\n  e.g. Paris.\n\
             - `units`: metric or imperial.\n\
             * days \u{2014} how far ahead.\n\n# Examples\n\n* `ignored` - not an argument",
        );
        assert_eq!(parsed.description, "Look up.");
        assert_eq!(
            parsed.arguments,
            vec![
                ("city".to_string(), "The city, e.g. Paris.".to_string()),
                ("units".to_string(), "metric or imperial.".to_string()),
                ("days".to_string(), "how far ahead.".to_string()),
            ]
        );
    }

    #[test]
    fn code_blocks_are_neither_description_nor_headings() {
        let parsed = docs("Add two numbers.\n```\n# let x = 1;\nadd(1, 2);\n```\nThen return.");
        assert_eq!(parsed.description, "Add two numbers. Then return.");
    }

    #[test]
    fn is_result_matches_any_path_ending_in_result() {
        assert!(is_result(&parse_quote!(Result<Value, ToolError>)));
        assert!(is_result(&parse_quote!(anyhow::Result<u32>)));
        assert!(is_result(&parse_quote!(std::io::Result<()>)));
        assert!(!is_result(&parse_quote!(SearchResult)));
        assert!(!is_result(&parse_quote!(Vec<Result<u8, String>>)));
    }

    #[test]
    fn is_option_accepts_std_core_paths() {
        assert!(is_option(&parse_quote!(Option<String>)));
        assert!(is_option(&parse_quote!(option::Option<String>)));
        assert!(is_option(&parse_quote!(std::option::Option<String>)));
        assert!(is_option(&parse_quote!(core::option::Option<String>)));
        assert!(is_option(&parse_quote!(::std::option::Option<String>)));
        assert!(is_option(&parse_quote!(::core::option::Option<String>)));
    }

    #[test]
    fn is_option_rejects_lookalikes() {
        assert!(!is_option(&parse_quote!(String)));
        assert!(!is_option(&parse_quote!(Vec<Option<String>>)));
        assert!(!is_option(&parse_quote!(my::Option<String>)));
        assert!(!is_option(&parse_quote!(my::option::Option<String>)));
        assert!(!is_option(&parse_quote!(::option::Option<String>)));
        assert!(!is_option(&parse_quote!(<T as Trait>::Option)));
    }
}
