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
use syn::{parse_macro_input, FnArg, ItemFn, LitStr, Pat, PatType, ReturnType, Type, TypePath};

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
/// Generated code references `serde`, `schemars`, `serde_json`, `async_trait`,
/// and `gemini_adk_rs` via absolute (`::`-rooted) paths in *your* crate graph.
/// Consumers of `gemini-adk-rs` already have all of these as dependencies, so no
/// extra setup is required.
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
            if is_option(ty) {
                quote! {
                    #[serde(default)]
                    #ident: #ty,
                }
            } else {
                quote! { #ident: #ty }
            }
        });

    // Destructure the args struct into the original parameter bindings, then
    // forward them positionally into the inner impl fn.
    let destructure = &field_idents;
    let forward_args = &field_idents;

    let expanded = quote! {
        // Hidden args struct: drives both deserialization and schema generation.
        #[derive(::serde::Deserialize, ::schemars::JsonSchema)]
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

        #[::async_trait::async_trait]
        impl ::gemini_adk_rs::tool::ToolFunction for #tool_struct {
            fn name(&self) -> &str {
                #fn_name_str
            }

            fn description(&self) -> &str {
                #description
            }

            fn parameters(&self) -> ::core::option::Option<::serde_json::Value> {
                let root = ::schemars::schema_for!(#args_struct);
                ::core::option::Option::Some(
                    ::serde_json::to_value(root)
                        .expect("schemars schema should serialize to JSON"),
                )
            }

            async fn call(
                &self,
                args: ::serde_json::Value,
            ) -> ::core::result::Result<::serde_json::Value, ::gemini_adk_rs::error::ToolError> {
                let #args_struct { #(#destructure),* } =
                    ::serde_json::from_value(args).map_err(|e| {
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

/// Returns `true` if `ty` is syntactically an `Option<...>`.
fn is_option(ty: &Type) -> bool {
    if let Type::Path(TypePath { qself: None, path }) = ty {
        if let Some(seg) = path.segments.last() {
            return seg.ident == "Option";
        }
    }
    false
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
