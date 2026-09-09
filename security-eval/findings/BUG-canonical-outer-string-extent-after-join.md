# Outer string extent cache outlived a dynamic join

Status: focused probe green; landing pending.

`typed_errors::url_typed_errors_are_matchable_and_bridge_to_string` failed
closed with `InvalidExtent` for the `suffix` parameter of `url.split_suffix`.
The function joins an alias of that parameter with a freshly allocated
substring. Edge transport renames the result's count into a join parameter;
the unchanged outer reference's extent can consequently differ between
incoming states. The validator correctly drops the conflicting counted fact.

Lowering retained the outer string's pre-join cached length, so later reads
did not reconstruct the extent through its authenticated header.

After an `if` join with dynamic extent lanes, lowering now reloads the length
of live bound strings through the existing header load and size guard. List
logical-length/capacity lanes are unchanged. No validator rule, ownership
permission, or scalar equality rule is relaxed.

The original URL typed-error probe passes with this change. This is coverage
evidence for the repaired path, not a compiler-wide soundness claim.
