# BUG: canonical source required a runtime ABI for generic templates

The canonical callable-witness binder included unspecialized generic templates
in the runtime-function manifest. Type checking intentionally gives such a
template no finalized physical signature; only its checked concrete
specializations have runtime ABIs. As a result, valid checked code such as a
generic identity called at `Int` failed before SafeWIR lowering with:

```text
canonical runtime function `main.identity` lacks a finalized signature
```

The binder now excludes only declarations whose signatures still contain free
type variables. Concrete generated instances have no free variables and retain
the fail-closed requirement for an exact checked body and signature. The
focused generic callable-witness test covers the regression.
