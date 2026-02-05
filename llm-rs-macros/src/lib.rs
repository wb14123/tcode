//! Proc-macro crate for llm-rs tool definitions.
//!
//! Provides the `#[tool]` attribute macro for defining LLM tools from functions.

use convert_case::{Case, Casing};
use proc_macro::TokenStream;
use proc_macro_crate::{crate_name, FoundCrate};
use quote::{format_ident, quote};
use syn::{
    parse::{Parse, ParseStream},
    parse_macro_input, spanned::Spanned, Attribute, FnArg, ItemFn, Lit, Pat, PatType, Token,
};

/// Parsed attributes for the #[tool] macro.
struct ToolAttrs {
    /// Optional timeout in milliseconds.
    timeout_ms: Option<u64>,
}

impl Parse for ToolAttrs {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let mut timeout_ms = None;

        while !input.is_empty() {
            let ident: syn::Ident = input.parse()?;
            input.parse::<Token![=]>()?;

            match ident.to_string().as_str() {
                "timeout_ms" => {
                    let lit: Lit = input.parse()?;
                    match lit {
                        Lit::Int(lit_int) => {
                            timeout_ms = Some(lit_int.base10_parse()?);
                        }
                        _ => {
                            return Err(syn::Error::new(
                                lit.span(),
                                "timeout_ms must be an integer (milliseconds)",
                            ));
                        }
                    }
                }
                other => {
                    return Err(syn::Error::new(
                        ident.span(),
                        format!("unknown attribute: {}", other),
                    ));
                }
            }

            // Consume optional comma
            if input.peek(Token![,]) {
                input.parse::<Token![,]>()?;
            }
        }

        Ok(ToolAttrs { timeout_ms })
    }
}

/// Determine the crate path to use for `llm_rs`.
fn get_crate_path() -> proc_macro2::TokenStream {
    match crate_name("llm-rs") {
        Ok(FoundCrate::Itself) => quote! { crate },
        Ok(FoundCrate::Name(name)) => {
            let ident = format_ident!("{}", name);
            quote! { ::#ident }
        }
        Err(_) => quote! { ::llm_rs },
    }
}

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
/// # Timeout
///
/// Specify a timeout in milliseconds for the tool execution:
///
/// ```ignore
/// #[tool(timeout_ms = 30000)]
/// fn slow_tool(query: String) -> impl Stream<Item = Result<String, String>> {
///     // This tool has a 30-second timeout
///     tokio_stream::once(Ok("result".to_string()))
/// }
/// ```
///
/// # Async Streams
///
/// For async operations, return an async stream using `async_stream::stream!`:
///
/// ```ignore
/// #[tool]
/// fn fetch_data(url: String) -> impl Stream<Item = String> {
///     async_stream::stream! {
///         let result = do_async_work().await;
///         yield result;
///     }
/// }
/// ```
#[proc_macro_attribute]
pub fn tool(attr: TokenStream, item: TokenStream) -> TokenStream {
    let input_fn = parse_macro_input!(item as ItemFn);

    // Parse the attribute for timeout
    let attrs = if attr.is_empty() {
        ToolAttrs { timeout_ms: None }
    } else {
        match syn::parse::<ToolAttrs>(attr) {
            Ok(attrs) => attrs,
            Err(e) => return e.to_compile_error().into(),
        }
    };

    // Auto-detect crate path
    let crate_path = get_crate_path();

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

    // Reject async functions - tools should return lazy async streams instead
    if input_fn.sig.asyncness.is_some() {
        return syn::Error::new(
            input_fn.sig.asyncness.span(),
            "#[tool] does not support async functions. \
             Return an async stream (e.g., `async_stream::stream! { ... }`) instead.",
        )
        .to_compile_error()
        .into();
    }

    // Get function visibility
    let vis = &input_fn.vis;

    // Generate the tool constructor function
    let tool_fn_name = format_ident!("{}_tool", fn_name);

    let handler_body = quote! {
        |params: #params_struct_name| {
            #fn_name(#(params.#field_names),*)
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

    // Generate the tool creation code with optional timeout
    let timeout_expr = if let Some(timeout_ms) = attrs.timeout_ms {
        quote! { Some(::std::time::Duration::from_millis(#timeout_ms)) }
    } else {
        quote! { None }
    };

    let tool_creation = quote! {
        #crate_path::tool::Tool::new(
            #fn_name_str,
            #description,
            #timeout_expr,
            #handler_body,
        )
    };

    let output = quote! {
        // Generated params struct
        #[derive(::serde::Deserialize, ::schemars::JsonSchema)]
        #vis struct #params_struct_name {
            #(#struct_fields),*
        }

        // Original function (with cleaned params)
        #(#other_fn_attrs)*
        #vis #fn_unsafety #fn_abi fn #fn_name #fn_generics (#(#clean_inputs),*) #fn_output #block

        // Tool constructor function
        #vis fn #tool_fn_name() -> #crate_path::tool::Tool {
            #tool_creation
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
