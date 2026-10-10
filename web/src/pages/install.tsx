import {useEffect, useState} from 'react';
import type {ReactNode} from 'react';
import Link from '@docusaurus/Link';
import useBaseUrl from '@docusaurus/useBaseUrl';
import Layout from '@theme/Layout';
import Heading from '@theme/Heading';
import styles from './install.module.css';

type Architecture = 'x86_64' | 'aarch64';
type Method = 'appimage' | 'apt' | 'dnf' | 'zypper';
type Asset = {name: string; browser_download_url: string};
type Release = {tag_name: string; assets: Asset[]};
type ClientHints = {
  platform: string;
  getHighEntropyValues: (hints: string[]) => Promise<{architecture?: string; bitness?: string}>;
};

const releasesUrl = 'https://github.com/ChaoticTrials/Ephervex/releases';
const latestReleaseUrl = 'https://api.github.com/repos/ChaoticTrials/Ephervex/releases/latest';
const appImageUpdateUrl = (architecture: Architecture) =>
  `https://github.com/ChaoticTrials/Ephervex/releases/latest/download/Ephervex-${architecture}.AppImage`;
const methods: {id: Method; title: string; detail: string; icons: string[]; tag: string}[] = [
  {id: 'appimage', title: 'AppImage', detail: 'One file. No package setup.', icons: ['appimage'], tag: 'Works across distributions'},
  {id: 'apt', title: 'Debian & Ubuntu', detail: 'Install and update with APT.', icons: ['debian', 'ubuntu'], tag: 'Automatic updates'},
  {id: 'dnf', title: 'Fedora & DNF', detail: 'Install and update with DNF.', icons: ['fedora'], tag: 'Automatic updates'},
  {id: 'zypper', title: 'openSUSE', detail: 'Install a release RPM with Zypper.', icons: ['opensuse'], tag: 'Release package'},
];

function architectureFromBrowser(): Promise<Architecture | null> {
  const userAgent = navigator.userAgent;
  const clientHints = (navigator as Navigator & {userAgentData?: ClientHints}).userAgentData;
  const linux = clientHints?.platform === 'Linux' || (/Linux/i.test(userAgent) && !/Android/i.test(userAgent));
  if (!linux) return Promise.resolve(null);

  const fromLegacyAgent = (): Architecture | null => {
    if (/\b(?:aarch64|arm64)\b/i.test(userAgent)) return 'aarch64';
    if (/\b(?:x86_64|amd64)\b/i.test(userAgent)) return 'x86_64';
    return null;
  };

  if (!clientHints?.getHighEntropyValues) return Promise.resolve(fromLegacyAgent());
  return clientHints.getHighEntropyValues(['architecture', 'bitness'])
    .then(({architecture, bitness}) => {
      if (bitness === '64' && architecture === 'arm') return 'aarch64';
      if (bitness === '64' && architecture === 'x86') return 'x86_64';
      return fromLegacyAgent();
    })
    .catch(fromLegacyAgent);
}

function releaseAsset(release: Release | null, kind: 'appimage' | 'deb' | 'rpm', architecture: Architecture | null): Asset | null {
  if (!release || !architecture) return null;
  const debArchitecture = architecture === 'x86_64' ? 'amd64' : 'arm64';
  const pattern = kind === 'appimage'
    ? new RegExp(`^Ephervex-${architecture}\\.AppImage$`)
    : kind === 'deb'
      ? new RegExp(`^ephervex_[A-Za-z0-9.+:~_-]+_${debArchitecture}\\.deb$`)
      : new RegExp(`^ephervex-[A-Za-z0-9.+:~_-]+-1\\.${architecture}\\.rpm$`);
  return release.assets.find((asset) => pattern.test(asset.name)
    && asset.browser_download_url.startsWith('https://github.com/ChaoticTrials/Ephervex/releases/download/')) ?? null;
}

function Command({children, copyLabel = 'command'}: {children: string; copyLabel?: string}): ReactNode {
  const [copied, setCopied] = useState(false);

  async function copy() {
    try {
      await navigator.clipboard.writeText(children);
      setCopied(true);
      window.setTimeout(() => setCopied(false), 2000);
    } catch {
      setCopied(false);
    }
  }

  return <div className={styles.command}>
    <pre><code>{children}</code></pre>
    <button type="button" onClick={copy} aria-label={`Copy ${copyLabel}`}>{copied ? 'Copied' : 'Copy'}</button>
  </div>;
}

function Step({number, title, children}: {number: string; title: string; children: ReactNode}): ReactNode {
  return <div className={styles.step}>
    <span className={styles.stepNumber} aria-hidden="true">{number}</span>
    <div><Heading as="h3">{title}</Heading>{children}</div>
  </div>;
}

export default function Install(): ReactNode {
  const [method, setMethod] = useState<Method>('appimage');
  const [architecture, setArchitecture] = useState<Architecture | null>(null);
  const [suggestedArchitecture, setSuggestedArchitecture] = useState<Architecture | null>(null);
  const [release, setRelease] = useState<Release | null>(null);
  const [releaseChecked, setReleaseChecked] = useState(false);
  const iconBase = useBaseUrl('/img/install/');

  useEffect(() => {
    let active = true;
    architectureFromBrowser().then((suggestion) => {
      if (!active || !suggestion) return;
      setSuggestedArchitecture(suggestion);
      setArchitecture((current) => current ?? suggestion);
    });
    return () => { active = false; };
  }, []);

  useEffect(() => {
    const controller = new AbortController();
    fetch(latestReleaseUrl, {headers: {Accept: 'application/vnd.github+json'}, signal: controller.signal})
      .then((response) => {
        if (!response.ok) throw new Error('Release unavailable');
        return response.json() as Promise<Release>;
      })
      .then((latest) => {
        if (Array.isArray(latest.assets)) setRelease(latest);
      })
      .catch(() => {})
      .finally(() => { if (!controller.signal.aborted) setReleaseChecked(true); });
    return () => controller.abort();
  }, []);

  const appImage = releaseAsset(release, 'appimage', architecture);
  const deb = releaseAsset(release, 'deb', architecture);
  const rpm = releaseAsset(release, 'rpm', architecture);
  const selected = methods.find((item) => item.id === method)!;
  const download = method === 'appimage' ? appImage : method === 'apt' ? deb : rpm;
  const checksum = download && release?.assets.find((asset) => asset.name === `${download.name}.sha256`
    && asset.browser_download_url.startsWith('https://github.com/ChaoticTrials/Ephervex/releases/download/'));
  const fallbackDebName = architecture ? `ephervex_VERSION_${architecture === 'x86_64' ? 'amd64' : 'arm64'}.deb` : null;
  const fallbackRpmName = architecture ? `ephervex-VERSION-1.${architecture}.rpm` : null;

  return <Layout title="Install" description="Install Ephervex on Linux with an AppImage, APT, DNF, or a release RPM.">
    <main className={styles.page}>
      <section className={styles.hero}>
        <div className="container">
          <span className={styles.kicker}>EPHERVEX / LINUX</span>
          <Heading as="h1">Install Ephervex.</Heading>
          <p>Choose a package for your Linux machine, then follow the steps below. Every option includes the desktop app and CLI.</p>
        </div>
      </section>

      <div className={`container ${styles.content}`}>
        <section className={styles.chooser} aria-labelledby="choose-method">
          <div className={styles.sectionHeading}>
            <div><span className={styles.overline}>01 / PACKAGE METHOD</span><Heading as="h2" id="choose-method">How do you want to install?</Heading></div>
            <p>Choose the option that matches your distribution. The AppImage runs without adding a package source.</p>
          </div>
          <div className={styles.methods} role="group" aria-label="Package method">
            {methods.map((item) => <button
              className={`${styles.method} ${method === item.id ? styles.methodSelected : ''}`}
              type="button"
              key={item.id}
              aria-pressed={method === item.id}
              onClick={() => setMethod(item.id)}
            >
              <span className={styles.methodTop}>
                <span className={styles.methodIcons}>{item.icons.map((icon) => <img key={icon} src={`${iconBase}${icon}.svg`} alt="" />)}</span>
                <span className={styles.methodCheck} aria-hidden="true">{method === item.id ? '✓' : '↗'}</span>
              </span>
              <strong>{item.title}</strong><span className={styles.methodDetail}>{item.detail}</span>
              <span className={styles.methodTag}>{item.tag}</span>
            </button>)}
          </div>
        </section>

        <section className={styles.architecture} aria-labelledby="choose-architecture">
          <div><span className={styles.overline}>02 / ARCHITECTURE</span><Heading as="h2" id="choose-architecture">Check your machine.</Heading>
            <p>{suggestedArchitecture
              ? <>Your browser suggests <code>{suggestedArchitecture}</code>. Confirm it on the Linux machine with </>
              : <>Choose the architecture of the Linux machine you will use. Check it with </>}
              <code>uname -m</code>.</p>
          </div>
          <fieldset className={styles.archChoices}>
            <legend className={styles.visuallyHidden}>Linux architecture</legend>
            {(['x86_64', 'aarch64'] as const).map((option) => <label key={option} className={architecture === option ? styles.archSelected : ''}>
              <input type="radio" name="architecture" value={option} checked={architecture === option} onChange={() => setArchitecture(option)} />
              <span>{option}</span>{suggestedArchitecture === option && <small>Browser suggestion</small>}
            </label>)}
          </fieldset>
        </section>

        <section className={styles.instructions} aria-labelledby="install-steps">
          <div className={styles.instructionsHeader}>
            <div><span className={styles.overline}>03 / INSTALL</span><Heading as="h2" id="install-steps">{selected.title}</Heading><p>{selected.detail}</p></div>
            {release?.tag_name && <span className={styles.releaseTag}>Latest release: {release.tag_name}</span>}
          </div>

          {method === 'apt' && <div className={styles.steps}>
            <Step number="01" title="Add the package repository">
              <p>This lets APT install Ephervex and include it in future updates.</p>
              <Command>{'curl -fsSL https://repo.chaotictrials.de/apt-keyring.gpg \\\n  | sudo tee /usr/share/keyrings/chaotictrials.gpg > /dev/null\necho "deb [signed-by=/usr/share/keyrings/chaotictrials.gpg] \\\nhttps://repo.chaotictrials.de/deb stable main" \\\n  | sudo tee /etc/apt/sources.list.d/chaotictrials.list > /dev/null'}</Command>
            </Step>
            <Step number="02" title="Install Ephervex"><Command>{'sudo apt update\nsudo apt install ephervex'}</Command></Step>
          </div>}

          {method === 'dnf' && <div className={styles.steps}>
            <Step number="01" title="Add the package repository">
              <p>On Fedora and other distributions with DNF 5:</p>
              <Command>{'sudo dnf config-manager addrepo --from-repofile=https://repo.chaotictrials.de/rpm/chaotictrials.repo'}</Command>
              <details className={styles.alternative}><summary>Using DNF 4?</summary><Command>{'sudo dnf config-manager --add-repo https://repo.chaotictrials.de/rpm/chaotictrials.repo'}</Command></details>
            </Step>
            <Step number="02" title="Install Ephervex"><Command>{'sudo dnf install ephervex'}</Command><p>DNF will include Ephervex in later package updates.</p></Step>
          </div>}

          {method === 'appimage' && <div className={styles.steps}>
            <Step number="01" title="Download the AppImage">
              <p>Keep the file wherever you want to run Ephervex. It does not install a package.</p>
              <Download asset={download} checked={releaseChecked} architecture={architecture} appImage />
            </Step>
            <Step number="02" title="Make it executable and run it">
              {architecture ? <Command>{`chmod +x Ephervex-${architecture}.AppImage\n./Ephervex-${architecture}.AppImage`}</Command> : <p>Choose an architecture above to see the exact filename and commands.</p>}
              <p>If FUSE is unavailable, prefix the run command with <code>APPIMAGE_EXTRACT_AND_RUN=1</code>.</p>
            </Step>
          </div>}

          {method === 'appimage' && <div className={styles.updateSource}>
            <div>
              <strong>Update URL for AppImage managers</strong>
              <p>If your AppImage manager accepts a static update URL, paste this link for the architecture you chose. The same link is used by the download button above and points to the latest release.</p>
              <p><a href="https://gearlever.mijorus.it/docs/static-updates/">Gear Lever's Static URL instructions</a> describe where to use it.</p>
            </div>
            {architecture ? <Command copyLabel="update URL">{appImageUpdateUrl(architecture)}</Command>
              : <p>Choose an architecture above to see its update URL.</p>}
          </div>}

          {method === 'zypper' && <div className={styles.steps}>
            <Step number="01" title="Download the release RPM">
              <p>Choose the RPM for your architecture from the latest release.</p>
              <Download asset={download} checked={releaseChecked} architecture={architecture} />
            </Step>
            <Step number="02" title="Install the downloaded file">
              {architecture ? <Command>{`sudo zypper install ./${rpm?.name ?? fallbackRpmName}`}</Command> : <p>Choose an architecture above to see the right package filename.</p>}
              {!rpm && architecture && <p>Replace <code>VERSION</code> with the version in the downloaded filename.</p>}
              <p>A downloaded RPM does not add a repository for future upgrades.</p>
            </Step>
          </div>}

          {(method === 'apt' || method === 'dnf') && <div className={styles.directPackage}>
            <div><strong>Want a single package file?</strong><p>Download the release package instead. A direct package install does not add the update repository.</p></div>
            <Download asset={download} checked={releaseChecked} architecture={architecture} />
            {architecture && <Command>{method === 'apt' ? `sudo apt install ./${deb?.name ?? fallbackDebName}` : `sudo dnf install ./${rpm?.name ?? fallbackRpmName}`}</Command>}
            {!download && architecture && <p>Replace <code>VERSION</code> with the version in the downloaded filename.</p>}
          </div>}

          {download && checksum && <div className={styles.verify}>
            <div><strong>Verify the download</strong><p>Save the matching checksum next to the downloaded file, then check it before running or installing.</p></div>
            <a href={checksum.browser_download_url}>Download .sha256</a>
            <Command>{`sha256sum --check ${checksum.name}`}</Command>
          </div>}
        </section>

        <section className={styles.next} aria-labelledby="after-install">
          <div><span className={styles.overline}>04 / START</span><Heading as="h2" id="after-install">Open Ephervex.</Heading><p>Start the desktop app, or check your setup from the command line.</p></div>
          <div className={styles.nextCommands}>
            <div><strong>Desktop</strong><p>{method === 'appimage' ? 'Run the AppImage without arguments:' : 'Open Ephervex from your app menu, or run:'}</p><Command>{method === 'appimage' ? `./Ephervex-${architecture ?? 'YOUR_ARCH'}.AppImage` : 'ephervex-gui'}</Command></div>
            <div><strong>Command line</strong><p>{method === 'appimage' ? 'Run the AppImage with a CLI command:' : 'Check your local setup:'}</p><Command>{method === 'appimage' ? `./Ephervex-${architecture ?? 'YOUR_ARCH'}.AppImage doctor` : 'ephervex doctor'}</Command></div>
          </div>
          <Link to="/docs/v1/interface-tour">See what you can do next <span aria-hidden="true">→</span></Link>
        </section>

        <section className={styles.requirements} id="requirements" aria-labelledby="requirements-title">
          <div><span className={styles.overline}>BEFORE YOU BEGIN</span><Heading as="h2" id="requirements-title">Requirements & help</Heading></div>
          <div className={styles.requirementGrid}>
            <div><strong>Linux, 64-bit</strong><p><code>x86_64</code> or <code>aarch64</code> with glibc 2.35 or newer, such as Ubuntu 22.04 or Debian 12. X11 or XWayland is needed for the desktop app.</p></div>
            <div><strong>AppImage & FUSE</strong><p>The AppImage needs FUSE. If it cannot mount, run it with <code>APPIMAGE_EXTRACT_AND_RUN=1</code> before the command instead.</p></div>
            <div><strong>ARM game launches</strong><p>On <code>aarch64</code>, the Minecraft version must provide compatible native libraries. Some versions cannot launch on ARM.</p></div>
          </div>
          <p className={styles.helpLine}>Need another package or an older release? <a href={releasesUrl}>Browse all releases</a>.</p>
        </section>
      </div>
    </main>
  </Layout>;
}

function Download({asset, checked, architecture, appImage = false}: {asset: Asset | null; checked: boolean; architecture: Architecture | null; appImage?: boolean}): ReactNode {
  if (!architecture) return <p className={styles.downloadHint}>Choose your architecture above to find the right file.</p>;
  return <div className={styles.downloadActions}>
    {asset || appImage ? <a className={styles.downloadButton} href={appImage ? appImageUpdateUrl(architecture) : asset?.browser_download_url}>Download {asset?.name ?? `Ephervex-${architecture}.AppImage`} <span aria-hidden="true">↓</span></a>
      : <a className={styles.downloadButton} href={releasesUrl}>Browse releases <span aria-hidden="true">↗</span></a>}
    {!asset && !appImage && <span>{checked ? 'Pick the file matching your architecture.' : 'Checking the latest release...'}</span>}
    {appImage && <a className={styles.allReleases} href={releasesUrl}>All releases</a>}
  </div>;
}
