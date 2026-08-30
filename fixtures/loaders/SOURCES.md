# Forge and NeoForge loader fixtures

Captured and reduced on 2026-08-08. Only fields exercised by Ephervex are
retained; full installer binaries are not redistributed.

- `forge-promotions.json`: https://files.minecraftforge.net/net/minecraftforge/forge/promotions_slim.json
- `forge-maven-metadata.xml`: https://maven.minecraftforge.net/net/minecraftforge/forge/maven-metadata.xml
- `neoforge-maven-metadata.xml`: https://maven.neoforged.net/releases/net/neoforged/neoforge/maven-metadata.xml
- Forge 1.12.2 installer: `forge-1.12.2-14.23.5.2859-installer.jar`, SHA-1 `6b3d5be8b58d5385fda9c4d285367f4cb6e697b2`
- Forge 1.16.3 installer: `forge-1.16.3-34.1.42-installer.jar`, SHA-1 `10e3c85aa71f9f3b92a2c129c66f312e74e0c6df`
- Forge 1.20.1 installer: `forge-1.20.1-47.4.10-installer.jar`, SHA-1 `66bfea9963bfa60d88bab6b2750e74a958392715`
- NeoForge 26.1.2 installer: `neoforge-26.1.2.94-installer.jar`, SHA-1 `3bcef6cc370a6db9dde9741724bdc455b14fc304`
- NeoForge 26.2.0.59 installer: `neoforge-26.2.0.59-installer.jar`, SHA-1 `cabc4ecd3c2eccce92d1f4c048d643b16eea584e`

The four `*-install-profile.json` files, `forge-1.16.3-version.json`,
`forge-1.20.1-version.json`, and `neoforge-26.2.0.59-version.json` are reduced
from those exact official installer archives. They retain the identity, format discriminator, embedded
versus remote artifact shape, side filtering, token syntax, launch-sensitive
JVM arguments, and representative processor tasks; omitted entries are
repetitions of those same wire shapes.

The Forge 1.16.3 pair is the ModLauncher era, and it is retained because it
is the one that contradicts the obvious reading of a profile. Its
`install_profile.json` declares `spec` 0 while carrying a full four-step
processor chain, so `spec` cannot say whether an install has to patch and remap
the game. Its `data` and `processors` are complete; only the profile's library
list is reduced. `forge-1.16.3-version.json` keeps the launcher jar the era
declares with an empty URL and a real digest (the file ships inside the
installer under `maven/` and no processor produces it), alongside one remote
library the omitted entries repeat.

`neoforge-26.2.0.59-version.json` is the pairing that matters for version
identity: a loader build number under the calendar Minecraft scheme, whose own
id (`neoforge-26.2.0.59`) is not a Minecraft version and whose `inheritsFrom`
(`26.2`) is. Its 25 further libraries are omitted; the one retained carries the
remote-artifact shape the others repeat.

Installer profile observations are independently implemented from the
published data and the LGPL Forge installer sources. Synthetic ZIPs are used
for executable processor tests.
