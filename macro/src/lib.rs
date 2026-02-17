use proc_macro::TokenStream;
use proc_macro2::{Ident, TokenStream as ProcMacroTokenStream};
use quote::quote_spanned;
use syn::{parse2, ItemFn};

/// Register a benchmark function with Iai's test-framework harness.
///
/// The macro parses a function item and emits:
/// - The original function unchanged.
/// - A non-generic wrapper that passes the function through `black_box`.
/// - A `test_case` registration tuple linking a benchmark name to the wrapper.
#[proc_macro_attribute]
pub fn iai(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let item = proc_macro2::TokenStream::from(item);

    let span = proc_macro2::Span::call_site();

    let function_name = find_name(item.clone());
    let wrapper_function_name = Ident::new(&format!("wrap_{}", function_name), span);
    let const_name = Ident::new(&format!("IAI_FUNC_{}", function_name), span);
    let name_literal = function_name.to_string();

    let output = quote_spanned!(span=>
        #item

        fn #wrapper_function_name() {
            let _ = iai::black_box(#function_name());
        }

        #[test_case]
        const #const_name : (&'static str, fn()) = (#name_literal, #wrapper_function_name);
    );

    output.into()
}

fn find_name(stream: ProcMacroTokenStream) -> Ident {
    // Parse the input as a typed function syntax tree so name extraction is
    // deterministic and does not rely on token heuristics.
    // Panics early with a diagnostic when `#[iai]` is not applied to a function
    // item, which is the contract of this attribute macro.
    let function: ItemFn = parse2(stream)
        .unwrap_or_else(|error| panic!("`#[iai]` attribute expects a function item: {}", error));

    function.sig.ident
}
