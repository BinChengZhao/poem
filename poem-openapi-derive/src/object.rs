use darling::{
    ast::Data,
    util::{Ignored, SpannedValue},
    FromDeriveInput, FromField,
};
use proc_macro2::{Ident, TokenStream};
use quote::quote;
use syn::{ext::IdentExt, Attribute, DeriveInput, Error, Generics, Path, Type};

use crate::{
    common_args::{ConcreteType, DefaultValue, ExternalDocument, RenameRule, RenameRuleExt},
    error::GeneratorResult,
    utils::{get_crate_name, get_description, optional_literal},
    validators::Validators,
};

#[derive(FromField)]
#[darling(attributes(oai), forward_attrs(doc))]
struct ObjectField {
    ident: Option<Ident>,
    ty: Type,
    attrs: Vec<Attribute>,

    #[darling(default)]
    skip: bool,

    #[darling(default)]
    rename: Option<String>,
    #[darling(default)]
    default: Option<DefaultValue>,
    #[darling(default)]
    write_only: bool,
    #[darling(default)]
    read_only: bool,
    #[darling(default)]
    validator: Option<Validators>,
    #[darling(default)]
    flatten: bool,
}

#[derive(FromDeriveInput)]
#[darling(attributes(oai), forward_attrs(doc))]
struct ObjectArgs {
    ident: Ident,
    generics: Generics,
    attrs: Vec<Attribute>,
    data: Data<Ignored, ObjectField>,

    #[darling(default)]
    internal: bool,
    #[darling(default)]
    inline: SpannedValue<bool>,
    #[darling(default)]
    rename: Option<String>,
    #[darling(default)]
    rename_all: Option<RenameRule>,
    #[darling(default, multiple, rename = "concrete")]
    concretes: Vec<ConcreteType>,
    #[darling(default)]
    deprecated: bool,
    #[darling(default)]
    read_only_all: bool,
    #[darling(default)]
    write_only_all: bool,
    #[darling(default)]
    example: Option<SpannedValue<Path>>,
    #[darling(default)]
    deny_unknown_fields: bool,
    #[darling(default)]
    external_docs: Option<ExternalDocument>,
}

pub(crate) fn generate(args: DeriveInput) -> GeneratorResult<TokenStream> {
    let args: ObjectArgs = ObjectArgs::from_derive_input(&args)?;
    let crate_name = get_crate_name(args.internal);
    let (impl_generics, ty_generics, where_clause) = args.generics.split_for_impl();
    let ident = &args.ident;
    let s = match &args.data {
        Data::Struct(s) => s,
        _ => {
            return Err(
                Error::new_spanned(ident, "Object can only be applied to an struct.").into(),
            );
        }
    };
    let oai_typename = args.rename.clone().unwrap_or_else(|| ident.to_string());
    let description = get_description(&args.attrs)?;
    let mut deserialize_fields = Vec::new();
    let mut serialize_fields = Vec::new();
    let mut register_types = Vec::new();
    let mut fields = Vec::new();
    let mut meta_fields = Vec::new();
    let mut required_fields = Vec::new();

    if *args.inline && !args.concretes.is_empty() {
        return Err(Error::new(
            args.inline.span(),
            "Inline objects cannot have the `concretes` attribute.",
        )
        .into());
    }

    if args.example.is_some() && !args.concretes.is_empty() {
        return Err(Error::new(
            args.example.as_ref().unwrap().span(),
            "The example should be specified with the `concretes.example` attribute.",
        )
        .into());
    }

    for field in &s.fields {
        let field_ident = field.ident.as_ref().unwrap();
        let field_ty = &field.ty;
        let read_only = args.read_only_all || field.read_only;
        let write_only = args.write_only_all || field.write_only;

        if field.skip {
            deserialize_fields.push(quote! {
                let #field_ident: #field_ty = ::std::default::Default::default();
            });
            fields.push(field_ident);
            continue;
        }

        if read_only && write_only {
            return Err(Error::new_spanned(
                field_ident,
                "The `write_only` and `read_only` attributes cannot be enabled both.",
            )
            .into());
        }

        let field_name = field
            .rename
            .clone()
            .unwrap_or_else(|| args.rename_all.rename(field_ident.unraw().to_string()));
        let field_description = get_description(&field.attrs)?;
        let field_description = optional_literal(&field_description);
        let validators = field.validator.clone().unwrap_or_default();
        let validators_checker = validators.create_obj_field_checker(&crate_name, &field_name)?;
        let validators_update_meta = validators.create_update_meta(&crate_name)?;

        fields.push(field_ident);

        if read_only {
            deserialize_fields.push(quote! {
                #[allow(non_snake_case)]
                let #field_ident: #field_ty = {
                    if obj.contains_key(#field_name) {
                        return Err(#crate_name::types::ParseError::custom(format!("properties `{}` is read only.", #field_name)));
                    }
                    ::std::default::Default::default()
                };
            });
        } else if !field.flatten {
            match &field.default {
                Some(default_value) => {
                    let default_value = match default_value {
                        DefaultValue::Default => {
                            quote!(<#field_ty as ::std::default::Default>::default())
                        }
                        DefaultValue::Function(func_name) => quote!(#func_name()),
                    };

                    deserialize_fields.push(quote! {
                        #[allow(non_snake_case)]
                        let #field_ident: #field_ty = {
                            match obj.remove(#field_name) {
                                ::std::option::Option::Some(#crate_name::__private::serde_json::Value::Null) | ::std::option::Option::None => #default_value,
                                value => {
                                    let value = #crate_name::types::ParseFromJSON::parse_from_json(value).map_err(#crate_name::types::ParseError::propagate)?;
                                    #validators_checker
                                    value
                                }
                            }
                        };
                    });
                }
                _ => {
                    deserialize_fields.push(quote! {
                        #[allow(non_snake_case)]
                        let #field_ident: #field_ty = {
                            let value = #crate_name::types::ParseFromJSON::parse_from_json(obj.remove(#field_name))
                                .map_err(#crate_name::types::ParseError::propagate)?;
                            #validators_checker
                            value
                        };
                    });
                }
            };
        } else {
            deserialize_fields.push(quote! {
                #[allow(non_snake_case)]
                let #field_ident: #field_ty = {
                    #crate_name::types::ParseFromJSON::parse_from_json(::std::option::Option::Some(#crate_name::__private::serde_json::Value::Object(::std::clone::Clone::clone(&obj))))
                        .map_err(#crate_name::types::ParseError::propagate)?
                };
            });
        }

        if !field.flatten {
            if !write_only {
                serialize_fields.push(quote! {
                    if let ::std::option::Option::Some(value) = #crate_name::types::ToJSON::to_json(&self.#field_ident) {
                        object.insert(::std::string::ToString::to_string(#field_name), value);
                    }
                });
            }
        } else {
            serialize_fields.push(quote! {
                if let ::std::option::Option::Some(#crate_name::__private::serde_json::Value::Object(obj)) = #crate_name::types::ToJSON::to_json(&self.#field_ident) {
                    object.extend(obj);
                }
            });
        }

        let field_meta_default = match &field.default {
            Some(DefaultValue::Default) => {
                quote!(#crate_name::types::ToJSON::to_json(&<#field_ty as ::std::default::Default>::default()))
            }
            Some(DefaultValue::Function(func_name)) => {
                quote!(#crate_name::types::ToJSON::to_json(&#func_name()))
            }
            None => quote!(::std::option::Option::None),
        };

        if !field.flatten {
            register_types
                .push(quote!(<#field_ty as #crate_name::types::Type>::register(registry);));

            meta_fields.push(quote! {{
                let original_schema = <#field_ty as #crate_name::types::Type>::schema_ref();
                let patch_schema = {
                    let mut schema = #crate_name::registry::MetaSchema::ANY;
                    schema.default = #field_meta_default;
                    schema.read_only = #read_only;
                    schema.write_only = #write_only;

                    if let ::std::option::Option::Some(field_description) = #field_description {
                        schema.description = ::std::option::Option::Some(field_description);
                    }
                    #validators_update_meta
                    schema
                };

                fields.push((#field_name, original_schema.merge(patch_schema)));
            }});

            let has_default = field.default.is_some();
            required_fields.push(quote! {
                if <#field_ty>::IS_REQUIRED && !#has_default {
                    fields.push(#field_name);
                }
            });
        } else {
            meta_fields.push(quote! {
                fields.extend(registry.create_fake_schema::<#field_ty>().properties);
            });
            required_fields.push(quote! {
                fields.extend(registry.create_fake_schema::<#field_ty>().required);
            });
        }
    }

    let description = optional_literal(&description);
    let deprecated = args.deprecated;
    let external_docs = match &args.external_docs {
        Some(external_docs) => {
            let s = external_docs.to_token_stream(&crate_name);
            quote!(::std::option::Option::Some(#s))
        }
        None => quote!(::std::option::Option::None),
    };
    let meta = quote! {
        #crate_name::registry::MetaSchema {
            description: #description,
            external_docs: #external_docs,
            required: {
                #[allow(unused_mut)]
                let mut fields = ::std::vec::Vec::new();
                #(#required_fields)*
                fields
            },
            properties: {
                let mut fields = ::std::vec::Vec::new();
                #(#meta_fields)*
                fields
            },
            deprecated: #deprecated,
            ..#crate_name::registry::MetaSchema::new("object")
        }
    };
    let deny_unknown_fields = if args.deny_unknown_fields {
        Some(quote! {
            if let ::std::option::Option::Some((field_name, _)) = std::iter::Iterator::next(&mut ::std::iter::IntoIterator::into_iter(obj)) {
                return Err(#crate_name::types::ParseError::custom(format!("unknown field `{}`.", field_name)));
            }
        })
    } else {
        None
    };

    let expanded = if args.concretes.is_empty() {
        let example = match &args.example {
            Some(path) => {
                let path = &**path;
                quote! { <Self as #impl_generics #crate_name::types::ToJSON>::to_json(&#path()) }
            }
            None => quote!(::std::option::Option::None),
        };

        let (fn_schema_ref, fn_register) = if *args.inline {
            (
                quote!(#crate_name::registry::MetaSchemaRef::Inline(Box::new({
                    let mut meta = #meta;
                    meta.example = #example;
                    meta
                }))),
                quote! {
                    #(#register_types)*
                },
            )
        } else {
            (
                quote!(#crate_name::registry::MetaSchemaRef::Reference(#oai_typename)),
                quote! {
                    registry.create_schema::<Self, _>(#oai_typename, |registry| {
                        #(#register_types)*
                        let mut meta = #meta;
                        meta.example = #example;
                        meta
                    })
                },
            )
        };

        quote! {
            impl #impl_generics #crate_name::types::Type for #ident #ty_generics #where_clause {
                const IS_REQUIRED: bool = true;

                type RawValueType = Self;

                type RawElementValueType = Self;

                fn name() -> ::std::borrow::Cow<'static, str> {
                    ::std::convert::Into::into(#oai_typename)
                }

                fn schema_ref() -> #crate_name::registry::MetaSchemaRef {
                    #fn_schema_ref
                }

                fn register(registry: &mut #crate_name::registry::Registry) {
                    #fn_register
                }

                fn as_raw_value(&self) -> ::std::option::Option<&Self::RawValueType> {
                    ::std::option::Option::Some(self)
                }

                fn raw_element_iter<'a>(&'a self) -> ::std::boxed::Box<dyn ::std::iter::Iterator<Item = &'a Self::RawElementValueType> + 'a> {
                    ::std::boxed::Box::new(::std::iter::IntoIterator::into_iter(self.as_raw_value()))
                }
            }

            impl #impl_generics #crate_name::types::ParseFromJSON for #ident #ty_generics #where_clause {
                fn parse_from_json(value: ::std::option::Option<#crate_name::__private::serde_json::Value>) -> ::std::result::Result<Self, #crate_name::types::ParseError<Self>> {
                    let value = value.unwrap_or_default();
                    match value {
                        #crate_name::__private::serde_json::Value::Object(mut obj) => {
                            #(#deserialize_fields)*
                            #deny_unknown_fields
                            ::std::result::Result::Ok(Self { #(#fields),* })
                        }
                        _ => ::std::result::Result::Err(#crate_name::types::ParseError::expected_type(value)),
                    }
                }
            }

            impl #impl_generics #crate_name::types::ToJSON for #ident #ty_generics #where_clause {
                fn to_json(&self) -> ::std::option::Option<#crate_name::__private::serde_json::Value> {
                    let mut object = #crate_name::__private::serde_json::Map::new();
                    #(#serialize_fields)*
                    ::std::option::Option::Some(#crate_name::__private::serde_json::Value::Object(object))
                }
            }
        }
    } else {
        let mut code = Vec::new();

        code.push(quote! {
            impl #impl_generics #ident #ty_generics #where_clause {
                fn __internal_create_schema(registry: &mut #crate_name::registry::Registry) -> #crate_name::registry::MetaSchema
                where
                    Self: #crate_name::types::Type
                {
                    #(#register_types)*
                    #meta
                }

                fn __internal_parse_from_json(value: ::std::option::Option<#crate_name::__private::serde_json::Value>) -> ::std::result::Result<Self, #crate_name::types::ParseError<Self>> where Self: #crate_name::types::Type {
                    let value = value.unwrap_or_default();
                    match value {
                        #crate_name::__private::serde_json::Value::Object(mut obj) => {
                            #(#deserialize_fields)*
                            #deny_unknown_fields
                            ::std::result::Result::Ok(Self { #(#fields),* })
                        }
                        _ => ::std::result::Result::Err(#crate_name::types::ParseError::expected_type(value)),
                    }
                }

                fn __internal_to_json(&self) -> #crate_name::__private::serde_json::Value where Self: #crate_name::types::Type {
                    let mut object = ::serde_json::Map::new();
                    #(#serialize_fields)*
                    #crate_name::__private::serde_json::Value::Object(object)
                }
            }
        });

        for concrete in &args.concretes {
            let oai_typename = &concrete.name;
            let params = &concrete.params.0;
            let concrete_type = quote! { #ident<#(#params),*> };
            let example = match &concrete.example {
                Some(path) => {
                    quote! { <Self as #crate_name::types::ToJSON>::to_json(&#path()) }
                }
                None => quote!(::std::option::Option::None),
            };

            let expanded = quote! {
                impl #crate_name::types::Type for #concrete_type {
                    const IS_REQUIRED: bool = true;

                    type RawValueType = Self;

                    type RawElementValueType = Self;

                    fn name() -> ::std::borrow::Cow<'static, str> {
                        ::std::convert::Into::into(#oai_typename)
                    }

                    fn as_raw_value(&self) -> Option<&Self::RawValueType> {
                        ::std::option::Option::Some(self)
                    }

                    fn schema_ref() -> #crate_name::registry::MetaSchemaRef {
                        #crate_name::registry::MetaSchemaRef::Reference(#oai_typename)
                    }

                    fn register(registry: &mut #crate_name::registry::Registry) {
                        let mut meta = Self::__internal_create_schema(registry);
                        meta.example = #example;
                        registry.create_schema::<Self, _>(#oai_typename, move |registry| meta);
                    }

                    fn raw_element_iter<'a>(&'a self) -> ::std::boxed::Box<dyn ::std::iter::Iterator<Item = &'a Self::RawElementValueType> + 'a> {
                        ::std::boxed::Box::new(::std::iter::IntoIterator::into_iter(self.as_raw_value()))
                    }
                }

                impl #crate_name::types::ParseFromJSON for #concrete_type {
                    fn parse_from_json(value: ::std::option::Option<#crate_name::__private::serde_json::Value>) -> ::std::result::Result<Self, #crate_name::types::ParseError<Self>> {
                        Self::__internal_parse_from_json(value)
                    }
                }

                impl #crate_name::types::ToJSON for #concrete_type {
                    fn to_json(&self) -> ::std::option::Option<#crate_name::__private::serde_json::Value> {
                        ::std::option::Option::Some(Self::__internal_to_json(self))
                    }
                }
            };
            code.push(expanded);
        }

        quote!(#(#code)*)
    };

    Ok(expanded)
}
