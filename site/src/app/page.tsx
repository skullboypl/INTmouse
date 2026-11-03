import { DeskSimulator } from "@/components/desk-simulator";
import { INTmouseMark } from "@/components/intmouse-mark";
import { HeroIntro } from "@/components/reveal";
import { latestRelease } from "@/lib/release";
import { REPO_URL } from "@/lib/site";

// The page is static and rebuilt in the background at most once an hour; the only live
// data is the newest GitHub release, which is cached the same way. Next needs a literal
// here, so keep it equal to RELEASE_REVALIDATE_SECONDS.
export const revalidate = 3600;

const NAV = [
  { href: "#jak", label: "Jak działa" },
  { href: "#funkcje", label: "Funkcje" },
  { href: "#za-darmo", label: "Za darmo" },
  { href: "#pobierz", label: "Pobierz" },
  { href: "#faq", label: "Pytania" },
];

const FEATURES = [
  {
    title: "Mysz w grach",
    text: "W grze kursor rusza się tak, jak na jednym komputerze. Na drugi ekran przeskakuje tylko raz, w chwili przejścia.",
  },
  {
    title: "Mysz nie czeka na pliki",
    text: "Kopiowanie tekstu albo wysłanie pliku nie zatrzymuje ruchu myszy ani klawiatury.",
  },
  {
    title: "Bez przypadkowego przeskoku",
    text: "Przejście można wyłączyć w pełnym ekranie, pominąć narożniki, wymagać wciśniętego klawisza, zablokować w wybranych programach albo puścić dopiero po drugim dotknięciu krawędzi.",
  },
  {
    title: "Układ ekranów",
    text: "Ustawiasz, który monitor stoi obok którego. Z ostatniego da się wrócić na pierwszy, różne rozdzielczości się wyrównują, a skrót klawiszowy przerzuca kursor na wybrany komputer.",
  },
  {
    title: "Schowek i lampki klawiatury",
    text: "Tekst skopiowany na jednym komputerze wklejasz na drugim. Caps Lock, Num Lock i Scroll Lock ustawiają się tak samo po obu stronach.",
  },
  {
    title: "Hasło i klawisz awaryjny",
    text: "Komputery łączą się wspólnym hasłem. Możesz wskazać, które z nich mają prawo się połączyć. Jeden klawisz od razu oddaje sterowanie. Nieaktywny ekran przygasa, a program zostaje w ikonie obok zegara.",
  },
] as const;

const OPEN = [
  ["0 zł", "Za darmo, bez limitów", "Wszystkie komputery i monitory, wszystkie funkcje. Bez okresu próbnego, bez wersji Pro."],
  ["Bez konta", "Bez logowania i bez reklam", "Pobierasz plik i uruchamiasz. Nie ma rejestracji, panelu ani ekranu z prośbą o e-mail."],
  ["GPL", "Kod otwarty", "Czytasz, zmieniasz i rozdajesz dalej, a zmiany zostają otwarte. Licencja GPL. Błędy i pomysły zgłaszasz na GitHubie."],
  ["LAN", "Ruch zostaje u ciebie", "Mysz i klawiatura idą między komputerami w twojej sieci. Do internetu wychodzi tylko sprawdzenie aktualizacji, które możesz wyłączyć."],
] as const;

const FAQ = [
  {
    q: "Czy to naprawdę jest za darmo?",
    a: "Tak. Program jest darmowy i ma otwarty kod na licencji GPL. Nie ma wersji płatnej, okresu próbnego ani limitu komputerów.",
  },
  {
    q: "Jak działają aktualizacje?",
    a: "Kilka sekund po starcie i potem co 12 godzin program sprawdza plik version.json na tej stronie. Gdy jest nowsza wersja, na górze okna pojawia się pasek z przyciskami Aktualizuj i Później. Pobrany plik jest sprawdzany sumą SHA-256, a poprzednia wersja zostaje do przywrócenia. Sprawdzanie możesz wyłączyć w programie.",
  },
  {
    q: "Czy potrzebuję konta?",
    a: "Nie. Pobierasz plik z GitHuba i uruchamiasz go. Ta strona też nie ma logowania.",
  },
  {
    q: "Czy muszę wpisywać adresy IP?",
    a: "Nie. Ustaw to samo hasło na obu komputerach i włącz „Wymagaj hasła”. Program sam znajdzie drugi komputer w sieci lokalnej i się z nim połączy. Monitory drugiego komputera pojawiają się w układzie ekranów same. Adresy IP wpisujesz tylko wtedy, gdy chcesz ograniczyć połączenia do wybranych komputerów.",
  },
  {
    q: "Czy da się tym grać?",
    a: "Tak. Mysz w grze rusza się normalnie, a w pełnym ekranie możesz zatrzymać przejście na drugi komputer. Nie podajemy opóźnienia ani liczby klatek, bo jeszcze tego nie zmierzyliśmy.",
  },
  {
    q: "Na jakim systemie działa?",
    a: "Dziś na Windows. Linux i macOS są w planach, ale terminu jeszcze nie ma. Jeśli chcesz pomóc, kod czeka na GitHubie.",
  },
  {
    q: "Co program wysyła do internetu?",
    a: "Ruch myszy i klawiatury zostaje między komputerami w twojej sieci. Jedyne zapytanie do internetu to sprawdzenie aktualizacji (plik version.json z tej strony) i da się je wyłączyć. Strona i program nie zbierają statystyk ani danych o komputerze.",
  },
  {
    q: "Czy połączenie jest bezpieczne?",
    a: "Komputery łączą się wspólnym hasłem. Możesz dodatkowo wskazać, z których adresów w sieci wolno się łączyć. Używaj programu w sieci, której ufasz, na przykład w domu.",
  },
  {
    q: "To wersja beta?",
    a: "Tak. Program działa, ale jest we wczesnej fazie i nie był jeszcze szeroko testowany na wielu zestawach komputerów. Dlatego błędy zgłoszone na GitHubie są dla nas cenne.",
  },
];

const btn =
  "inline-flex items-center justify-center rounded-full bg-accent px-5 py-3 text-sm font-medium text-accent-fg hover:bg-ink hover:text-bg";
const btnGhost =
  "inline-flex items-center justify-center rounded-full border border-line px-5 py-3 text-sm font-medium text-fg hover:border-fg";

const formatSize = (bytes: number) => `${(bytes / 1024 / 1024).toFixed(1).replace(".", ",")} MB`;

export default async function Home() {
  const release = await latestRelease();
  const download = release?.zip ?? release?.exe ?? null;
  const checksum = release?.exe?.sha256 ?? release?.zip?.sha256 ?? null;

  const moves = [
    ["01", "Zostaje jeden zestaw", "Mysz i klawiatura leżą przy tobie. Drugi komputer jest w tej samej sieci i nie potrzebuje własnej myszy."],
    ["02", "Kursor idzie za krawędź", "Nie ma przycisku ani pudełka pod biurkiem. Kursor sam wchodzi na sąsiedni ekran."],
    ["03", "Pracujesz już tam", "Klikasz i piszesz na drugim komputerze. W grze mysz rusza się normalnie. Pełny ekran może zatrzymać przejście."],
  ] as const;

  return (
    <>
      <a
        href="#tresc"
        className="sr-only focus:not-sr-only focus:fixed focus:left-4 focus:top-4 focus:z-50 focus:bg-accent focus:px-4 focus:py-2 focus:text-accent-fg"
      >
        Przejdź do treści
      </a>

      <div className="sheet">
        <header className="topbar">
          <a href="#" aria-label="INTmouse, początek strony">
            <INTmouseMark />
          </a>
          <nav className="topnav" aria-label="Główna nawigacja">
            {NAV.map((n) => (
              <a key={n.href} href={n.href}>
                {n.label}
              </a>
            ))}
          </nav>
          <div className="topbar-links">
            <a href={REPO_URL} rel="noopener">
              GitHub
            </a>
          </div>
        </header>

        <main id="tresc">
          <HeroIntro>
            <section className="hero-split">
              <div className="hero-copy">
                <p className="font-mono text-[0.72rem] uppercase tracking-[0.18em] text-muted">
                  Za darmo · open source · bez konta · dziś Windows
                </p>
                <h1 className="poster">
                  Jedna mysz
                  <span className="block">i klawiatura.</span>
                  <span className="mt-2 block text-accent">Kilka komputerów.</span>
                </h1>
                <p className="max-w-md text-lg text-muted">
                  <strong className="font-medium text-fg">INTmouse</strong> steruje kilkoma komputerami
                  jedną myszą i jedną klawiaturą. Przesuwasz kursor za krawędź ekranu i jesteś już
                  na następnym komputerze. Działa też w grach. Darmowy, z otwartym kodem.
                </p>
                <div className="flex flex-wrap gap-3">
                  <a href="#pobierz" className={btn}>
                    Pobierz za darmo
                  </a>
                  <a href={REPO_URL} className={btnGhost} rel="noopener">
                    Kod na GitHubie
                  </a>
                </div>
              </div>
              <div id="symulator" className="hero-stage">
                <div className="seam" aria-hidden="true">
                  <span>krawędź</span>
                </div>
                <DeskSimulator />
              </div>
            </section>
          </HeroIntro>

          <section id="jak">
            <div className="pad">
              <p className="font-mono text-[0.72rem] uppercase tracking-[0.22em] text-muted">Jak to działa</p>
              <h2 className="mt-3 max-w-xl font-display text-4xl font-medium tracking-tight md:text-5xl">
                Z jednego biurka na drugi komputer.
              </h2>
            </div>
            <ol>
              {moves.map(([n, title, text]) => (
                <li key={n} className="spec-row">
                  <span className="stroke">{n}</span>
                  <h3 className="font-display text-2xl font-medium tracking-tight">{title}</h3>
                  <p className="text-muted">{text}</p>
                </li>
              ))}
            </ol>
          </section>

          <section id="funkcje">
            <div className="pad">
              <p className="font-mono text-[0.72rem] uppercase tracking-[0.22em] text-muted">W programie</p>
              <h2 className="mt-3 font-display text-4xl font-medium tracking-tight md:text-5xl">Co już potrafi</h2>
            </div>
            <ul>
              {FEATURES.map((f, i) => (
                <li key={f.title} className="spec-row">
                  <span className="font-mono text-sm text-accent">{String(i + 1).padStart(2, "0")}</span>
                  <h3 className="font-display text-xl font-medium tracking-tight">{f.title}</h3>
                  <p className="text-muted">{f.text}</p>
                </li>
              ))}
            </ul>
          </section>

          <section id="za-darmo">
            <div className="pad">
              <p className="font-mono text-[0.72rem] uppercase tracking-[0.22em] text-muted">Za darmo i otwarcie</p>
              <h2 className="mt-3 max-w-3xl font-display text-4xl font-medium tracking-tight md:text-5xl">
                Bez płacenia, bez konta, z kodem do przeczytania.
              </h2>
            </div>
            <ul>
              {OPEN.map(([tag, title, text]) => (
                <li key={title} className="spec-row">
                  <span className="font-mono text-sm text-accent">{tag}</span>
                  <h3 className="font-display text-xl font-medium tracking-tight">{title}</h3>
                  <p className="text-muted">{text}</p>
                </li>
              ))}
            </ul>
          </section>

          <section id="pobierz" className="band">
            <div>
              <h2>Na Windows. Jeden plik.</h2>
              {release ? (
                <>
                  <p className="mt-4 max-w-md">
                    Wersja {release.tag}
                    {release.prerelease ? ", beta" : ""}.
                    {release.summary ? ` ${release.summary}` : ""}
                  </p>
                  {checksum ? (
                    <p className="mt-2 max-w-xl break-all font-mono text-xs">SHA-256 {checksum}</p>
                  ) : null}
                </>
              ) : (
                <p className="mt-4 max-w-md">
                  Pierwsze wydanie pojawi się tutaj razem z sumą kontrolną. Kod jest już otwarty, więc
                  możesz zbudować program sam.
                </p>
              )}
            </div>
            {release ? (
              <a href={download?.url ?? release.url} className="band-soon" rel="noopener">
                {download
                  ? `Pobierz ${release.tag}${download.size ? ` (${formatSize(download.size)})` : ""}`
                  : `Zobacz wydanie ${release.tag}`}
              </a>
            ) : (
              <a href={REPO_URL} className="band-soon" rel="noopener">
                Zobacz kod na GitHubie
              </a>
            )}
          </section>

          <section id="faq">
            <div className="pad">
              <p className="font-mono text-[0.72rem] uppercase tracking-[0.22em] text-muted">Pytania</p>
              <h2 className="mt-3 font-display text-4xl font-medium tracking-tight">Zanim pobierzesz</h2>
            </div>
            <div>
              {FAQ.map((f) => (
                <article key={f.q} className="faq-row">
                  <h3 className="font-medium">{f.q}</h3>
                  <p className="text-sm text-muted">{f.a}</p>
                </article>
              ))}
            </div>
          </section>
        </main>

        <footer className="foot">
          <INTmouseMark />
          <span>
            Open source na licencji GPL ·{" "}
            <a href={REPO_URL} className="hover:text-fg" rel="noopener">
              GitHub
            </a>
          </span>
          <a href="#" className="hover:text-fg">
            Do góry
          </a>
        </footer>
      </div>
    </>
  );
}
