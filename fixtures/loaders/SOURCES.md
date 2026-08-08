# Forge and NeoForge loader fixtures

Captured and reduced on 2026-08-08. Only fields exercised by Ephervex are
retained; full installer binaries are not redistributed.

- `forge-promotions.json`: https://files.minecraftforge.net/net/minecraftforge/forge/promotions_slim.json
- `forge-maven-metadata.xml`: https://maven.minecraftforge.net/net/minecraftforge/forge/maven-metadata.xml
- `neoforge-maven-metadata.xml`: https://maven.neoforged.net/releases/net/neoforged/neoforge/maven-metadata.xml
- Forge 1.12.2 installer: `forge-1.12.2-14.23.5.2859-installer.jar`, SHA-1 `6b3d5be8b58d5385fda9c4d285367f4cb6e697b2`
- Forge 1.20.1 installer: `forge-1.20.1-47.4.10-installer.jar`, SHA-1 `66bfea9963bfa60d88bab6b2750e74a958392715`
- NeoForge 26.1.2 installer: `neoforge-26.1.2.94-installer.jar`, SHA-1 `3bcef6cc370a6db9dde9741724bdc455b14fc304`

The three `*-install-profile.json` files and
`forge-1.20.1-version.json` are reduced from those exact official installer
archives. They retain the identity, format discriminator, embedded versus
remote artifact shape, side filtering, token syntax, launch-sensitive JVM
arguments, and representative processor tasks; omitted entries are repetitions
of those same wire shapes.

Installer profile observations are independently implemented from the
published data and the LGPL Forge installer sources. Synthetic ZIPs are used
for executable processor tests.
