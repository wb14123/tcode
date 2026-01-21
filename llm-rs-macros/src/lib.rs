//! Proc-macro crate for llm-rs tool definitions.
//!
//! Provides the `#[tool]` attribute macro for defining LLM tools from functions.

use convert_case::{Case, Casing};
use proc_macro::TokenStream;
use quote::{format_ident, quote};
use syn::{parse_macro_input, spanned::Spanned, Attribute, FnArg, ItemFn, Pat, PatType};

/// Attribute macro that transforms a function into an LLM tool.
///
/// # Usage
///
/// ```ignore
/// use llm_rs::tool;
///
/// /// Read a file's contents from the filesystem
/// #[tool]
/// fn read_file(
///     /// The file path to read
///     path: String,
///     /// Optional encoding (default: utf-8)
///     #[serde(default)]
///     encoding: Option<String>,
/// ) -> impl Stream<Item = String> {
///     tokio_stream::once(format!("Reading {}", path))
/// }
///
/// // Creates a `read_file_tool()` function that returns a Tool
/// let tool = read_file_tool();
/// ```
///
/// # Generated Code
///
/// The macro generates:
/// 1. A params struct with `Deserialize` and `JsonSchema` derives
/// 2. The original function (with doc comments stripped from params)
/// 3. A `{fn_name}_tool()` function that creates the Tool
///
/// # Using within llm-rs crate
///
/// When using the macro within the llm-rs crate itself, use the `crate` attribute:
///
/// ```ignore
/// #[tool(crate = crate)]
/// fn internal_tool(query: String) -> impl Stream<Item = String> {
///     // ...
/// }
/// ```
///
/// # Async Functions
///
/// Async functions are supported. The macro wraps them appropriately:
///
/// ```ignore
/// #[tool]
/// async fn fetch_data(url: String) -> impl Stream<Item = String> {
///     // async implementation
/// }
/// ```
#[proc_macro_attribute]
pub fn tool(attr: TokenStream, item: TokenStream) -> TokenStream {
    let input_fn = parse_macro_input!(item as ItemFn);

    // Parse the attribute for crate path override
    let crate_path = if attr.is_empty() {
        quote! { ::llm_rs }
    } else {
        let attr_str = attr.to_string();
        if attr_str.contains("crate") && attr_str.contains("crate::") {
            // crate = crate means use the local crate path
            quote! { crate }
        } else if attr_str.contains("crate") {
            // Try to parse as crate = some_path
            quote! { crate }
        } else {
            quote! { ::llm_rs }
        }
    };

    // Extract function name
    let fn_name = &input_fn.sig.ident;
    let fn_name_str = fn_name.to_string();

    // Generate params struct name (e.g., read_file -> ReadFileParams)
    let params_struct_name = format_ident!("{}Params", fn_name_str.to_case(Case::Pascal));

    // Extract doc comments from function for tool description
    let description = extract_doc_comments(&input_fn.attrs);

    // Extract parameters with their names, types, and attributes
    // Validate that all parameters are supported (no self, no complex patterns)
    let mut params = Vec::new();
    for arg in input_fn.sig.inputs.iter() {
        match arg {
            FnArg::Receiver(receiver) => {
                // self, &self, &mut self - not supported
                return syn::Error::new(
                    receiver.span(),
                    "#[tool] cannot be used on methods with `self` parameter. \
                     Use a free function instead.",
                )
                .to_compile_error()
                .into();
            }
            FnArg::Typed(pat_type) => {
                if let Pat::Ident(pat_ident) = &*pat_type.pat {
                    let name = &pat_ident.ident;
                    let ty = &pat_type.ty;
                    let all_attrs = &pat_type.attrs;
                    params.push((name.clone(), ty.clone(), all_attrs.clone()));
                } else {
                    // Complex pattern like (a, b), Point { x, y }, etc.
                    return syn::Error::new(
                        pat_type.pat.span(),
                        "#[tool] only supports simple parameter names. \
                         Destructuring patterns like `(a, b)` or `Point { x, y }` are not supported.",
                    )
                    .to_compile_error()
                    .into();
                }
            }
        }
    }

    // Generate struct fields with ALL attributes (doc + serde)
    let struct_fields = params.iter().map(|(name, ty, attrs)| {
        quote! {
            #(#attrs)*
            pub #name: #ty
        }
    });

    // Generate field names for destructuring
    let field_names: Vec<_> = params.iter().map(|(name, _, _)| name.clone()).collect();

    // Check if function is async
    let is_async = input_fn.sig.asyncness.is_some();

    // Get function visibility
    let vis = &input_fn.vis;

    // Generate the tool constructor function
    let tool_fn_name = format_ident!("{}_tool", fn_name);

    // Generate the handler call based on whether the function is async
    let handler_body = if is_async {
        quote! {
            |params: #params_struct_name| {
                ::async_stream::stream! {
                    let mut stream = #fn_name(#(params.#field_names.clone()),*).await;
                    while let Some(item) = ::tokio_stream::StreamExt::next(&mut stream).await {
                        yield item;
                    }
                }
            }
        }
    } else {
        quote! {
            |params: #params_struct_name| {
                #fn_name(#(params.#field_names),*)
            }
        }
    };

    // Build the original function without doc comments on parameters
    let other_fn_attrs: Vec<_> = input_fn
        .attrs
        .iter()
        .filter(|attr| !attr.path().is_ident("tool"))
        .collect();

    // Recreate the function signature with stripped parameter attributes
    let fn_generics = &input_fn.sig.generics;
    let fn_asyncness = &input_fn.sig.asyncness;
    let fn_unsafety = &input_fn.sig.unsafety;
    let fn_abi = &input_fn.sig.abi;
    let fn_output = &input_fn.sig.output;

    // Strip doc/serde attrs from function params (they go to the struct)
    let clean_inputs: Vec<_> = input_fn
        .sig
        .inputs
        .iter()
        .map(|arg| {
            if let FnArg::Typed(pat_type) = arg {
                // Remove all custom attributes from function params
                let clean_pat_type = PatType {
                    attrs: Vec::new(), // No attributes on func params
                    pat: pat_type.pat.clone(),
                    colon_token: pat_type.colon_token,
                    ty: pat_type.ty.clone(),
                };
                FnArg::Typed(clean_pat_type)
            } else {
                arg.clone()
            }
        })
        .collect();

    let block = &input_fn.block;

    let output = quote! {
        // Generated params struct
        #[derive(::serde::Deserialize, ::schemars::JsonSchema)]
        #vis struct #params_struct_name {
            #(#struct_fields),*
        }

        // Original function (with cleaned params)
        #(#other_fn_attrs)*
        #vis #fn_asyncness #fn_unsafety #fn_abi fn #fn_name #fn_generics (#(#clean_inputs),*) #fn_output #block

        // Tool constructor function
        #vis fn #tool_fn_name() -> #crate_path::tool::Tool {
            #crate_path::tool::Tool::new(
                #fn_name_str,
                #description,
                #handler_body,
            )
        }
    };

    output.into()
}

/// Extract doc comments from attributes and join them into a single string.
fn extract_doc_comments(attrs: &[Attribute]) -> String {
    attrs
        .iter()
        .filter_map(|attr| {
            if attr.path().is_ident("doc") {
                // Parse the doc attribute value
                if let syn::Meta::NameValue(meta) = &attr.meta {
                    if let syn::Expr::Lit(expr_lit) = &meta.value {
                        if let syn::Lit::Str(lit_str) = &expr_lit.lit {
                            return Some(lit_str.value());
                        }
                    }
                }
            }
            None
        })
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}
