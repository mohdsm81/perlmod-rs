use proc_macro2::{Ident, Span, TokenStream};

use quote::quote_spanned;
use syn::ext::IdentExt;
use syn::spanned::Spanned;
use syn::{Error, Meta};

use crate::attribs::FunctionAttrs;

pub struct XSub {
    pub rust_name: Ident,
    pub perl_name: Option<Ident>,
    pub xs_name: Ident,
    pub tokens: TokenStream,
    pub prototype: Option<String>,
}

enum ArgumentAttrType {
    /// Skip the deserializer for this argument.
    Raw,

    /// Call `TryFrom<&Value>::try_from` for this argument instead of deserializing it.
    TryFromRef,

    /// This is the `CV` pointer.
    CVPtr(Span),

    /// Slurp the remaining arguments (like an `@rest` at the end).
    /// This requires the parameter to implement `FromIterator<T: Deserialize>`.
    TrailingList(Span),
}

impl ArgumentAttrType {
    fn from_attr(attr: &syn::Attribute) -> Option<Self> {
        Self::from_path(attr.path())
    }

    fn from_path(path: &syn::Path) -> Option<Self> {
        if path.is_ident("raw") {
            return Some(Self::Raw);
        }

        if path.is_ident("try_from_ref") {
            return Some(Self::TryFromRef);
        }

        if path.is_ident("cv") {
            return Some(Self::CVPtr(path.span()));
        }

        if path.is_ident("list") {
            return Some(Self::TrailingList(path.span()));
        }

        None
    }

    fn is_same_variant_as(&self, other: &Self) -> bool {
        core::mem::discriminant(self) == core::mem::discriminant(other)
    }

    fn as_str(&self) -> &'static str {
        match self {
            Self::Raw => "raw",
            Self::TryFromRef => "try_from_ref",
            Self::CVPtr(_) => "cv",
            Self::TrailingList(_) => "list",
        }
    }
}

struct ArgumentAttr<'s> {
    attr_type: Option<ArgumentAttrType>,
    pat_type: &'s syn::PatType,
}

impl<'s> ArgumentAttr<'s> {
    fn new_from_fn_arg(arg: &'s mut syn::FnArg) -> Result<Self, Error> {
        match arg {
            syn::FnArg::Receiver(_) => {
                bail!(arg => "cannot export self-taking methods as xsubs");
            }
            syn::FnArg::Typed(pat_type) => {
                let mut attr_type: Option<ArgumentAttrType> = None;
                let mut has_err = false;

                let span = pat_type.span();

                pat_type.attrs.retain(|attr| {
                    let Some(got_attr_type) = ArgumentAttrType::from_attr(attr) else {
                        // Attribute has no matching attribute type => retain it
                        return true;
                    };

                    if !matches!(attr.meta, Meta::Path(_)) {
                        error!(&attr.meta => "attribute does not take any value or parameter");
                        has_err = true;
                        return false;
                    }

                    let Some(existing_attr_type) = attr_type.as_ref() else {
                        // No existing attribute type => assign and don't retain
                        attr_type = Some(got_attr_type);
                        return false;
                    };

                    if existing_attr_type.is_same_variant_as(&got_attr_type) {
                        let attr_str = existing_attr_type.as_str();
                        error!(span, "duplicate attribute '{attr_str}'");
                        has_err = true;
                        return false;
                    }

                    // At this point we have two differing attributes
                    error!(
                        span,
                        "`raw`, `try_from_ref`, `cv`, and `list` attributes are mutually exclusive"
                    );
                    has_err = true;
                    false
                });

                if has_err {
                    bail!(span, "failed to determine argument attribute");
                }

                Ok(Self {
                    attr_type,
                    pat_type,
                })
            }
        }
    }
}

fn extract_argument_code(
    arg_attr: &ArgumentAttr,
    span: Span,
    arguments_name: &Ident,
    extracted_name: &Ident,
    none_handling: TokenStream,
) -> TokenStream {
    match arg_attr.attr_type {
        Some(ArgumentAttrType::TrailingList(_)) => {
            quote_spanned! { span=>
                let #extracted_name = #arguments_name.map(::perlmod::Value::from);
            }
        }
        _ => {
            quote_spanned! { span=>
                let #extracted_name: ::perlmod::Value = match #arguments_name.next() {
                    Some(arg) => ::perlmod::Value::from(arg),
                    None => #none_handling
                };
            }
        }
    }
}

fn deserialized_argument_code(
    arg_attr: &ArgumentAttr,
    span: Span,
    arg_type: &syn::Type,
    deserialized_name: &Ident,
    extracted_name: Ident,
) -> TokenStream {
    match &arg_attr.attr_type {
        Some(ArgumentAttrType::Raw) => quote_spanned! { span=>
            let #deserialized_name = #extracted_name;
        },
        Some(ArgumentAttrType::TryFromRef) => quote_spanned! { span=>
            let #deserialized_name: #arg_type =
                match ::std::convert::TryFrom::try_from(&#extracted_name) {
                    Ok(arg) => arg,
                    Err(err) => {
                        return Err(::perlmod::Value::new_string(&format!("{err:#}\n"))
                            .into_mortal()
                            .into_raw());
                    }
                };
        },
        Some(ArgumentAttrType::TrailingList(_)) => quote_spanned! { span=>
            let #deserialized_name = {
                let _guard = ::perlmod::__private__::InParameterDeserialization::guard();
                match <#arg_type as ::perlmod::__private__::serde::Deserialize>::deserialize(
                    ::perlmod::__private__::serde::de::value::SeqDeserializer::new(
                        #extracted_name
                    )
                ) {
                    Ok(arg) => arg,
                    Err(err) => {
                        return Err(::perlmod::Value::new_string(&format!("{err:#}\n"))
                            .into_mortal()
                            .into_raw());
                    }
                }
            };
        },
        Some(ArgumentAttrType::CVPtr(_)) | None => quote_spanned! { span=>
            let #deserialized_name: #arg_type =
                match ::perlmod::from_ref_value(&#extracted_name) {
                    Ok(data) => data,
                    Err(err) => {
                        return Err(::perlmod::Value::new_string(&format!("{err:#}\n"))
                            .into_mortal()
                            .into_raw());
                    }
                };
        },
    }
}

struct Return {
    result: bool,
    value: ReturnValue,
}

enum ReturnValue {
    /// Return nothing. (This is different from returning an implicit undef!)
    None,

    /// Return a single element.
    Single,

    /// We support tuple return types. They act like "list" return types in perl.
    Tuple(usize),
}

pub fn handle_function(
    attr: FunctionAttrs,
    mut func: syn::ItemFn,
    mangled_package_name: Option<&str>,
    export_public: bool,
) -> Result<XSub, Error> {
    let span = func.sig.ident.span();

    if !func.sig.generics.params.is_empty() {
        bail!(&func.sig.generics => "generic functions cannot be exported as xsubs");
    }

    if func.sig.asyncness.is_some() {
        bail!(&func.sig.asyncness => "async fns cannot be exported as xsubs");
    }

    let name = func.sig.ident.unraw();
    let export_public = export_public.then_some(&func.vis);
    let xs_name = attr
        .xs_name
        .clone()
        .unwrap_or_else(|| match mangled_package_name {
            None => Ident::new(&format!("xs_{name}"), name.span()),
            Some(prefix) => Ident::new(&format!("xs_{prefix}_{name}"), name.span()),
        });
    let impl_xs_name = Ident::new(&format!("impl_xs_{name}"), name.span());

    let arguments_name = syn::Ident::new("args", name.span());

    let mut trailing_options = 0;
    let mut extract_arguments = TokenStream::new();
    let mut deserialized_arguments = TokenStream::new();
    let mut passed_arguments = TokenStream::new();
    let mut cv_arg_param = TokenStream::new();
    let mut had_list_param = false;
    for arg in &mut func.sig.inputs {
        let arg_attr = ArgumentAttr::new_from_fn_arg(arg)?;

        if let Some(ArgumentAttrType::TrailingList(list_span)) = arg_attr.attr_type {
            if had_list_param {
                bail!(list_span, "only 1 #[list] parameter allowed");
            }

            had_list_param = true;
        }

        let arg_name = match &*arg_attr.pat_type.pat {
            syn::Pat::Ident(ident) => {
                if ident.by_ref.is_some() {
                    bail!(ident => "xsub does not support by-ref parameters");
                }
                if ident.subpat.is_some() {
                    bail!(ident => "xsub does not support sub-patterns on parameters");
                }
                &ident.ident
            }
            _ => bail!(&arg_attr.pat_type.pat => "xsub does not support this kind of parameter"),
        };

        let arg_type = &*arg_attr.pat_type.ty;

        if let Some(ArgumentAttrType::CVPtr(cv_span)) = arg_attr.attr_type {
            if !cv_arg_param.is_empty() {
                bail!(cv_span, "only 1 'cv' parameter allowed");
            }
            cv_arg_param = quote_spanned! { span=> #arg_name: #arg_type };
            if passed_arguments.is_empty() {
                passed_arguments.extend(quote_spanned! { span=> #arg_name });
            } else {
                passed_arguments.extend(quote_spanned! { span=> , #arg_name });
            }
            continue;
        }

        let extracted_name = Ident::new(&format!("extracted_arg_{arg_name}"), arg_name.span());
        let deserialized_name =
            Ident::new(&format!("deserialized_arg_{arg_name}"), arg_name.span());

        let missing_message = syn::LitStr::new(
            &format!("missing required parameter: '{arg_name}'\n"),
            arg_name.span(),
        );

        let none_handling = if is_option_type(arg_type).is_some() {
            trailing_options += 1;
            quote_spanned! { span=> ::perlmod::Value::new_undef(), }
        } else if matches!(arg_attr.attr_type, Some(ArgumentAttrType::TrailingList(_))) {
            TokenStream::new()
        } else {
            // only count the trailing options;
            trailing_options = 0;
            quote_spanned! { span=>
                {
                    return Err(::perlmod::Value::new_string(#missing_message)
                        .into_mortal()
                        .into_raw());
                }
            }
        };

        extract_arguments.extend(extract_argument_code(
            &arg_attr,
            span,
            &arguments_name,
            &extracted_name,
            none_handling,
        ));

        deserialized_arguments.extend(deserialized_argument_code(
            &arg_attr,
            span,
            arg_type,
            &deserialized_name,
            extracted_name,
        ));

        if passed_arguments.is_empty() {
            passed_arguments.extend(quote_spanned! { span=> #deserialized_name });
        } else {
            passed_arguments.extend(quote_spanned! { span=> , #deserialized_name });
        }
    }

    let has_return_value = match &func.sig.output {
        syn::ReturnType::Default => Return {
            result: false,
            value: ReturnValue::None,
        },
        syn::ReturnType::Type(_arrow, ty) => match get_result_type(ty) {
            (syn::Type::Tuple(tuple), result) if tuple.elems.is_empty() => Return {
                result,
                value: ReturnValue::None,
            },
            (syn::Type::Tuple(tuple), result) => Return {
                result,
                value: ReturnValue::Tuple(tuple.elems.len()),
            },
            (_, result) => Return {
                result,
                value: ReturnValue::Single,
            },
        },
    };

    let finalize_arguments = if !had_list_param {
        let too_many_args_error = syn::LitStr::new(
            &format!(
                "too many parameters for function '{}', (expected {})\n",
                name,
                func.sig.inputs.len() - (!cv_arg_param.is_empty()) as usize
            ),
            Span::call_site(),
        );

        quote_spanned! { span=>
            if #arguments_name.next().is_some() {
                return Err(::perlmod::Value::new_string(#too_many_args_error)
                    .into_mortal()
                    .into_raw());
            }
        }
    } else {
        TokenStream::new()
    };

    let ReturnHandling {
        return_type,
        handle_return,
        wrapper_func,
    } = handle_return_kind(
        &attr,
        has_return_value,
        &name,
        &xs_name,
        &impl_xs_name,
        passed_arguments,
        export_public,
        !cv_arg_param.is_empty(),
    )?;

    let visibility_action = check_visibility(&func);

    let mut tokens = quote::quote! {
        #func

        #wrapper_func
    };

    tokens.extend(quote_spanned! { span=>
        #[inline(never)]
        #[allow(non_snake_case)]
        fn #impl_xs_name(#cv_arg_param) -> Result<#return_type, *mut ::perlmod::ffi::SV> {
            #visibility_action

            let argmark = unsafe { ::perlmod::ffi::pop_arg_mark() };
            let mut #arguments_name = argmark.iter();
            { let _ = &mut #arguments_name; }

            #extract_arguments

            #finalize_arguments

            #deserialized_arguments

            unsafe {
                argmark.set_stack();
            }

            let res = std::panic::catch_unwind(move || {
                #handle_return
            });
            match res {
                Ok(res) => res,
                Err(_panic) => Err(::perlmod::Value::new_string("rust function panicked")
                    .into_mortal()
                    .into_raw()),
            }
        }
    });

    Ok(XSub {
        rust_name: name,
        perl_name: attr.perl_name,
        xs_name,
        tokens,
        prototype: attr.prototype.or_else(|| {
            Some(gen_prototype(
                func.sig.inputs.len(),
                trailing_options,
                had_list_param,
            ))
        }),
    })
}

fn gen_prototype(arg_count: usize, trailing_options: usize, had_list_param: bool) -> String {
    let arg_count = arg_count - trailing_options - (had_list_param as usize);

    let mut proto = String::with_capacity(arg_count + trailing_options + 1);

    for _ in 0..arg_count {
        proto.push('$');
    }
    if trailing_options > 0 {
        proto.push(';');
        for _ in 0..trailing_options {
            proto.push('$');
        }
        if had_list_param {
            proto.push('@');
        }
    } else if had_list_param {
        proto.push_str(";@");
    }
    proto
}

struct ReturnHandling {
    return_type: TokenStream,
    handle_return: TokenStream,
    wrapper_func: TokenStream,
}

#[allow(clippy::too_many_arguments)]
fn handle_return_kind(
    attr: &FunctionAttrs,
    ret: Return,
    name: &Ident,
    xs_name: &Ident,
    impl_xs_name: &Ident,
    passed_arguments: TokenStream,
    export_public: Option<&syn::Visibility>,
    cv_arg: bool,
) -> Result<ReturnHandling, Error> {
    let span = name.span();

    let return_type;
    let mut handle_return;
    let wrapper_func;

    let vis = match export_public {
        Some(vis) => quote_spanned! { span=> #[unsafe(no_mangle)] #vis },
        None => quote_spanned! { span=> #[allow(non_snake_case)] },
    };

    let (cv_arg_name, cv_arg_passed) = if cv_arg {
        (
            quote_spanned! { span=> cv },
            quote_spanned! { span=> ::perlmod::Value::from_raw_ref(cv as *mut ::perlmod::ffi::SV) },
        )
    } else {
        (quote_spanned! { span=> _cv }, TokenStream::new())
    };

    let return_error = ret.return_error_code(attr, name);
    let copy_errno = ret.copy_errno(attr, name);

    let pthx = crate::pthx_param();
    match ret.value {
        ReturnValue::None => {
            return_type = quote_spanned! { span=> () };

            if attr.raw_return {
                bail!(&attr.raw_return => "raw_return attribute is illegal without a return value");
            }

            if ret.result {
                handle_return = quote_spanned! { span=>
                    match #name(#passed_arguments) {
                        Ok(()) => (),
                        Err(err) => { #return_error }
                    }

                    Ok(())
                };
            } else {
                handle_return = quote_spanned! { span=>
                    #name(#passed_arguments);

                    Ok(())
                };
            }

            wrapper_func = quote_spanned! { span=>
                #[doc(hidden)]
                #vis extern "C" fn #xs_name(#pthx #cv_arg_name: *mut ::perlmod::ffi::CV) {
                    unsafe {
                        let res = #impl_xs_name(#cv_arg_passed);
                        #copy_errno
                        match res {
                            Ok(()) => (),
                            Err(sv) => ::perlmod::ffi::croak(sv),
                        }
                    }
                }
            };
        }
        ReturnValue::Single => {
            return_type = quote_spanned! { span=> () };

            if ret.result {
                handle_return = quote_spanned! { span=>
                    let result = match #name(#passed_arguments) {
                        Ok(output) => output,
                        Err(err) => { #return_error }
                    };
                };
            } else {
                handle_return = quote_spanned! { span=>
                    let result = #name(#passed_arguments);
                };
            }

            if attr.raw_return {
                handle_return.extend(quote_spanned! { span=>
                    unsafe {
                        ::perlmod::ffi::stack_push_raw(result.into_mortal().into_raw());
                    }
                    Ok(())
                });
            } else {
                handle_return.extend(quote_spanned! { span=>
                    match ::perlmod::ser::to_return_value(&result) {
                        Ok(rv) => Ok(rv.__private_push_to_stack()),
                        Err(err) => Err(::perlmod::Value::new_string(&format!("{err:#}\n"))
                            .into_mortal()
                            .into_raw()),
                    }
                });
            };

            wrapper_func = quote_spanned! { span=>
                #[doc(hidden)]
                #vis extern "C" fn #xs_name(#pthx #cv_arg_name: *mut ::perlmod::ffi::CV) {
                    unsafe {
                        let res = #impl_xs_name(#cv_arg_passed);
                        #copy_errno
                        match res {
                            Ok(()) => (),
                            Err(sv) => ::perlmod::ffi::croak(sv),
                        }
                    }
                }
            };
        }
        ReturnValue::Tuple(count) => {
            return_type = {
                let mut rt = TokenStream::new();
                for _ in 0..count {
                    rt.extend(quote_spanned! { span=> *mut ::perlmod::ffi::SV, });
                }
                quote_spanned! { span=> (#rt) }
            };

            if ret.result {
                handle_return = quote_spanned! { span=>
                    let result = match #name(#passed_arguments) {
                        Ok(output) => output,
                        Err(err) => { #return_error }
                    };
                };
            } else {
                handle_return = quote_spanned! { span=>
                    let result = #name(#passed_arguments);
                };
            }

            let mut rt = TokenStream::new();
            if attr.raw_return {
                for i in 0..count {
                    let i = simple_usize(i, Span::call_site());
                    rt.extend(quote_spanned! { span=> (result.#i).into_mortal().into_raw(), });
                }
            } else {
                for i in 0..count {
                    let i = simple_usize(i, Span::call_site());
                    rt.extend(quote_spanned! { span=>
                        match ::perlmod::to_value(&result.#i) {
                            Ok(value) => value.into_mortal().into_raw(),
                            Err(err) => return
                                Err(::perlmod::Value::new_string(&format!("{err:#}\n"))
                                    .into_mortal()
                                    .into_raw()),
                        },
                    });
                }
            }
            handle_return.extend(quote_spanned! { span=>
                Ok((#rt))
            });
            drop(rt);

            let icount = simple_usize(count, Span::call_site());
            let sp_offset = simple_usize(count - 1, Span::call_site());
            let mut push = quote_spanned! { span=>
                ::perlmod::ffi::RSPL_stack_resize_by(#icount);
                let mut sp = ::perlmod::ffi::RSPL_stack_sp().sub(#sp_offset);
                *sp = sv.0;
            };

            for i in 1..count {
                let i = simple_usize(i, Span::call_site());
                push.extend(quote_spanned! { span=>
                    sp = sp.add(1);
                    *sp = sv.#i;
                });
            }
            //let mut push = TokenStream::new();
            //for i in 0..count {
            //    let i = simple_usize(i, Span::call_site());
            //    push.extend(quote_spanned! { span=>
            //        ::perlmod::ffi::stack_push_raw(sv.#i);
            //    });
            //}

            wrapper_func = quote_spanned! { span=>
                #[doc(hidden)]
                #vis extern "C" fn #xs_name(#pthx #cv_arg_name: *mut ::perlmod::ffi::CV) {
                    unsafe {
                        let res = #impl_xs_name(#cv_arg_passed);
                        #copy_errno
                        match res {
                            Ok(sv) => { #push },
                            Err(sv) => ::perlmod::ffi::croak(sv),
                        }
                    }
                }
            };
        }
    }

    Ok(ReturnHandling {
        return_type,
        handle_return,
        wrapper_func,
    })
}

impl Return {
    fn return_error_code(&self, attr: &FunctionAttrs, name: &Ident) -> TokenStream {
        if !self.result {
            return TokenStream::new();
        }

        if attr.serialize_error {
            quote_spanned! { name.span() =>
                match ::perlmod::to_value(&err) {
                    Ok(err) => return Err(err.into_mortal().into_raw()),
                    Err(err) => {
                        return Err(::perlmod::Value::new_string(&format!("{err:#}\n"))
                            .into_mortal()
                            .into_raw());
                    }
                }
            }
        } else {
            quote_spanned! { name.span() =>
                return Err(::perlmod::Value::new_string(&format!("{err:#}\n"))
                    .into_mortal()
                    .into_raw());
            }
        }
    }

    fn copy_errno(&self, attr: &FunctionAttrs, name: &Ident) -> TokenStream {
        if attr.errno {
            quote_spanned! { name.span() => ::perlmod::error::copy_errno_to_libc(); }
        } else {
            TokenStream::new()
        }
    }
}

/// Note that we cannot handle renamed imports at all here...
pub fn is_result_type(ty: &syn::Type) -> Option<&syn::Type> {
    if let syn::Type::Path(p) = ty {
        if p.qself.is_some() {
            return None;
        }
        let segs = &p.path.segments;
        let is_result = match segs.len() {
            1 => segs[0].ident == "Result",
            3 => segs[0].ident == "std" && segs[1].ident == "result" && segs[2].ident == "Result",
            _ => false,
        };
        if !is_result {
            return None;
        }

        if let syn::PathArguments::AngleBracketed(generic) = &segs.last().unwrap().arguments {
            // We allow aliased Result types with an implicit Error:
            if generic.args.len() != 1 && generic.args.len() != 2 {
                return None;
            }

            if let syn::GenericArgument::Type(ty) = generic.args.first().unwrap() {
                return Some(ty);
            }
        }
    }
    None
}

/// If the type is a Result type, return the contained Ok type, otherwise return the type itself.
/// Also return whether or not it actually was a Result.
pub fn get_result_type(ty: &syn::Type) -> (&syn::Type, bool) {
    match is_result_type(ty) {
        Some(ty) => (ty, true),
        None => (ty, false),
    }
}

/// Get a non-suffixed integer from an usize.
fn simple_usize(i: usize, span: Span) -> syn::LitInt {
    syn::LitInt::new(&format!("{i}"), span)
}

/// Note that we cannot handle renamed imports at all here...
pub fn is_option_type(ty: &syn::Type) -> Option<&syn::Type> {
    if let syn::Type::Path(p) = ty {
        if p.qself.is_some() {
            return None;
        }
        let segs = &p.path.segments;
        let is_option = match segs.len() {
            1 => segs[0].ident == "Option",
            3 => segs[0].ident == "std" && segs[1].ident == "option" && segs[2].ident == "Option",
            _ => false,
        };
        if !is_option {
            return None;
        }

        if let syn::PathArguments::AngleBracketed(generic) = &segs.last().unwrap().arguments {
            if generic.args.len() != 1 {
                return None;
            }

            if let syn::GenericArgument::Type(ty) = generic.args.first().unwrap() {
                return Some(ty);
            }
        }
    }
    None
}

fn check_visibility(func: &syn::ItemFn) -> TokenStream {
    use crate::config::Action;

    if !matches!(func.vis, syn::Visibility::Inherited) {
        return TokenStream::new();
    }

    match crate::config::non_pub_exports() {
        Action::Allow => TokenStream::new(),
        Action::Warn => {
            let span = func.sig.ident.span();
            quote_spanned! {
                span=>
                {
                    non_pub_export();
                    #[deprecated = "exported function must be public"]
                    fn non_pub_export() {}
                }
            }
        }
        Action::Deny => {
            let span = func.sig.ident.span();
            quote_spanned! {
                span=> compile_error!("exported function must be public");
            }
        }
    }
}
