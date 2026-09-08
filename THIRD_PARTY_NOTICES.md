# Third-party notices

Pix Host does not vendor third-party source code. Rust and relay dependencies
are resolved from their package registries and retain the licenses declared by
their respective projects. The macOS client also links the pinned Sparkle
2.9.4 binary distribution, licensed under the MIT license; its upstream license
is recorded in each release's generated license report.

The implementation was informed by the MIT-licensed Remote Pi project. No
Remote Pi source is included in this repository unless a future change records
the copied file, copyright notice, and original license here.

Release jobs should regenerate the dependency license report from Cargo and
the relay lockfile before publishing an artifact.
