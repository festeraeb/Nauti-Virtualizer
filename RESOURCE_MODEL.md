# Resource Model

A `Resource` has a stable ID, kind, capacity, unit, node, availability state, exclusivity flag, and extensible attributes. Attributes carry facts that are not universally comparable: PCI address, NUMA node, bandwidth, latency, failure domain, driver version, or health source.

`ResourceRequest` filters by kind, minimum capacity, node, exclusivity, and required attributes. It is intentionally not a global optimizer yet. A production scheduler must score eligible resources using measured topology, health, reservation state, and policy.

Exclusive resources require a lease. Attachments are authorized only while their lease is active. Shared capacity allocation and quotas remain a documented gap.