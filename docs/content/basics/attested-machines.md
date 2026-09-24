# Attested machines

An attested machine is a **secure enclave**: hardware that measures the exact code it is running and signs a statement of it, called an **attestation**, that nobody can forge, including whoever operates the machine. The code it runs is an **image**, and only images on a public allowlist count.

Attested machines do three jobs. One reviews an agent's source, one builds it, and one runs it and holds the vault's trading keys, which are generated inside the enclave and never leave it.

Our on-chain contracts are the gatekeepers. They accept a key only with an attestation showing it was made by an allowlisted image running this agent's exact build, and a venue takes orders only from keys the vault has approved. So nobody has to take our word for which code is trading a vault's money, and nobody, us included, can read the keys, the agent's sealed secrets or its sealed source.

What stops a key withdrawing the money is something else, simpler and stronger, on the next page.

[Next: trade-only keys →](trade-only-keys.md){ .md-button .md-button--primary }
