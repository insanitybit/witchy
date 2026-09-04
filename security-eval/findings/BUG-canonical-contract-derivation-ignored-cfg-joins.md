# Canonical callable derivation ignored CFG joins

## Impact

The canonical SafeWIR callable-contract checker derived reference origins by
walking blocks in storage order. It did not propagate branch arguments into
destination block parameters. A reference returned, passed, or mutated after a
CFG join therefore lost its parameter or allocation origin, and the derived
contract could depend on block serialization rather than program control flow.

This was primarily fail-closed for result contracts, but block-order-dependent
authority derivation is not an acceptable security boundary.

## Fix

Derive origins with a monotone CFG fixed point before summarizing effects and
results. Entry parameters seed the analysis, branch arguments join into target
block parameters, incompatible origins become `Unknown`, and calls and escapes
consume the propagated origins.

The focused regression `returning_a_parameter_through_a_cfg_join_derives_alias_result`
pins an aliased parameter returned through a block parameter.
