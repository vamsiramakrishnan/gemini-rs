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
//! #[tool("Get the current weather for a city")]
//! async fn get_weather(city: String, units: Option<String>) -> Result<Value, ToolError> {
//!     Ok(json!({ "city": city, "units": units.unwrap_or("metric".into()) }))
//! }
//!
//! // `get_weather()` returns a value implementing `ToolFunction`.
//! let mut d = ToolDispatcher::new();
//! d.register_function(std::sync::Arc::new(get_weather()));
//! ```

use proc_macro::TokenStream;
use quote::{format_ident, quote};
use syn::{
    Data, DeriveInput, Expr, ExprLit, Fields, FnArg, ItemFn, Lit, LitInt, LitStr, Meta, Pat,
    PatType, ReturnType, Type, TypePath, parse_macro_input,
};

/// Turn an `async fn` into a registrable Gemini tool.
///
/// The attribute takes a single string literal — the tool's description, as
/// surfaced to the model:
///
/// ```ignore
/// #[tool("Get the current weather for a city")]
/// async fn get_weather(city: String, units: Option<String>) -> Result<Value, ToolError> {
///     Ok(json!({ "city": city, "units": units.unwrap_or("metric".into()) }))
/// }
/// ```
///
/// # What it generates
///
/// For a function `fn foo(...)`, the macro emits:
///
/// - A hidden args struct `__FooArgs` deriving `serde::Deserialize` and
///   `schemars::JsonSchema`, with one field per parameter. This drives both
///   argument deserialization and JSON-Schema generation.
/// - A hidden tool type `__FooTool` implementing
///   `gemini_adk_rs::tool::ToolFunction`:
///   - `name()` returns the function name (`"foo"`).
///   - `description()` returns the attribute string.
///   - `parameters()` returns the schemars-generated JSON Schema.
///   - `call(args)` deserializes `args` into `__FooArgs`, runs the original
///     function body, and returns its `Result<Value, ToolError>`.
/// - A public constructor `fn foo() -> __FooTool` (visibility matches the
///   original fn) that you register with a `gemini_adk_rs::tool::ToolDispatcher`:
///
/// ```ignore
/// dispatcher.register_function(std::sync::Arc::new(foo()));
/// ```
///
/// # Supported parameters
///
/// Any parameter type that is `serde::Deserialize + schemars::JsonSchema` is
/// supported. `Option<T>` parameters are optional in the schema. Zero-parameter
/// tools are supported (the generated schema is an empty object).
///
/// # Path hygiene
///
/// Generated code reaches `serde`, `schemars`, `serde_json`, and `async_trait`
/// through `::gemini_adk_rs::__macros` (the derives are pointed there with
/// `#[serde(crate = ..)]` / `#[schemars(crate = ..)]`), so none of them need
/// to be in your `Cargo.toml`. The expansion is rooted at `::gemini_adk_rs`,
/// which therefore must be a *direct* dependency of the crate using the macro —
/// a re-export through another crate (such as `gemini-adk-fluent-rs`) is not
/// enough.
///
/// # Follow-ups (not yet supported)
///
/// - Per-parameter doc descriptions are not extracted into the schema in v1.
///   Function parameters cannot carry doc comments in Rust, so this would
///   require a `#[doc = "..."]`-style attribute on each param.
#[proc_macro_attribute]
pub fn tool(attr: TokenStream, item: TokenStream) -> TokenStream {
    let description = parse_macro_input!(attr as LitStr);
    let func = parse_macro_input!(item as ItemFn);

    match expand(description, func) {
        Ok(ts) => ts.into(),
        Err(e) => e.to_compile_error().into(),
    }
}

fn expand(description: LitStr, func: ItemFn) -> syn::Result<proc_macro2::TokenStream> {
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
    let output = &sig.output;

    // Collect (ident, type) for each parameter; reject `self` receivers.
    let mut field_idents = Vec::new();
    let mut field_types = Vec::new();
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
                field_idents.push(ident);
                field_types.push((*ty).clone());
            }
        }
    }

    // The return type must be present (`-> Result<...>`); the body is reused
    // verbatim, so we just forward whatever the user wrote.
    let return_type: proc_macro2::TokenStream = match output {
        ReturnType::Default => {
            return Err(syn::Error::new_spanned(
                sig,
                "#[tool] requires a return type of `Result<serde_json::Value, ToolError>`",
            ));
        }
        ReturnType::Type(_, ty) => quote! { #ty },
    };

    // Naming for generated items, derived from the (Pascal-cased) fn name.
    let pascal = to_pascal_case(&fn_name.to_string());
    let args_struct = format_ident!("__{}Args", pascal);
    let tool_struct = format_ident!("__{}Tool", pascal);
    // The inner async fn that holds the original body, invoked from `call`.
    let inner_fn = format_ident!("__{}_impl", fn_name);

    let fn_name_str = fn_name.to_string();

    // Build the hidden args struct fields.
    let struct_fields = field_idents
        .iter()
        .zip(field_types.iter())
        .map(|(ident, ty)| {
            // `Option<T>` fields default to `None` when absent from the JSON.
            // No trailing comma here — `#(#struct_fields),*` adds the separators.
            if is_option(ty) {
                quote! {
                    #[serde(default)]
                    #ident: #ty
                }
            } else {
                quote! { #ident: #ty }
            }
        });

    // Destructure the args struct into the original parameter bindings, then
    // forward them positionally into the inner impl fn.
    let destructure = &field_idents;
    let forward_args = &field_idents;

    // Upstream crates are reached through `gemini_adk_rs::__macros` so the consumer
    // doesn't need them in scope under those exact names.
    let serde = quote! { ::gemini_adk_rs::__macros::serde };
    let schemars = quote! { ::gemini_adk_rs::__macros::schemars };
    let async_trait = quote! { ::gemini_adk_rs::__macros::async_trait };
    let serde_json = quote! { ::gemini_adk_rs::__macros::serde_json };

    let expanded = quote! {
        // Hidden args struct: drives both deserialization and schema generation.
        #[derive(#serde::Deserialize, #schemars::JsonSchema)]
        #[serde(crate = "gemini_adk_rs::__macros::serde")]
        #[schemars(crate = "gemini_adk_rs::__macros::schemars")]
        #[allow(non_camel_case_types, non_snake_case)]
        struct #args_struct {
            #(#struct_fields),*
        }

        // The original function body, preserved verbatim as a free async fn.
        #[allow(non_snake_case)]
        async fn #inner_fn ( #(#field_idents : #field_types),* ) -> #return_type #body

        // Hidden tool type implementing `ToolFunction`.
        #[allow(non_camel_case_types)]
        #vis struct #tool_struct;

        #[#async_trait::async_trait]
        impl ::gemini_adk_rs::tool::ToolFunction for #tool_struct {
            fn name(&self) -> &str {
                #fn_name_str
            }

            fn description(&self) -> &str {
                #description
            }

            fn parameters(&self) -> ::core::option::Option<#serde_json::Value> {
                let root = #schemars::schema_for!(#args_struct);
                ::core::option::Option::Some(
                    #serde_json::to_value(root)
                        .expect("schemars schema should serialize to JSON"),
                )
            }

            async fn call(
                &self,
                args: #serde_json::Value,
            ) -> ::core::result::Result<#serde_json::Value, ::gemini_adk_rs::error::ToolError> {
                let #args_struct { #(#destructure),* } =
                    #serde_json::from_value(args).map_err(|e| {
                        ::gemini_adk_rs::error::ToolError::InvalidArgs(
                            ::std::format!("Failed to deserialize arguments: {e}"),
                        )
                    })?;
                #inner_fn ( #(#forward_args),* ).await
            }
        }

        // Public constructor: `fn foo() -> __FooTool`.
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
            pub fn extract() -> ::gemini_adk_rs::extract::Extract {
                ::gemini_adk_rs::extract::Extract::record(#name)
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
        let mut confirm = quote! { ::gemini_adk_rs::frame::ConfirmPolicy::Never };
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
                        "never" => quote! { ::gemini_adk_rs::frame::ConfirmPolicy::Never },
                        "low_confidence" => {
                            quote! { ::gemini_adk_rs::frame::ConfirmPolicy::LowConfidence }
                        }
                        "always" => quote! { ::gemini_adk_rs::frame::ConfirmPolicy::Always },
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
            quote! { Some(::gemini_adk_rs::frame::SlotValidator::Range { min: #min_tok, max: #max_tok }) }
        } else if non_empty {
            quote! { Some(::gemini_adk_rs::frame::SlotValidator::NonEmpty) }
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
            ::gemini_adk_rs::frame::SlotSpec {
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
        impl ::gemini_adk_rs::frame::Frame for #ident {
            #[doc = #doc]
            fn frame() -> ::gemini_adk_rs::frame::FrameSpec {
                ::gemini_adk_rs::frame::FrameSpec {
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
    let r = quote! { ::gemini_adk_rs::extract::Recognizer };
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
    let r = quote! { ::gemini_adk_rs::frame::SlotRecognizer };
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
    use super::is_option;
    use syn::parse_quote;

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
