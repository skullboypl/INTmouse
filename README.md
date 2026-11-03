# IntMouse

Jedna mysz i jedna klawiatura na kilku komputerach w sieci domowej. Przesuwasz kursor za
krawędź ekranu i jesteś już na następnym komputerze. Darmowy, open source (GPL), bez konta.

*English: a free, open-source mouse and keyboard sharing tool for Windows PCs on a LAN
(a lightweight alternative to ShareMouse). UDP for input, TCP for handshake, clipboard and files.*

Strona: <https://intmouse.vxh.pl> · Wydania: [GitHub Releases](https://github.com/skullboypl/intmouse/releases)

## Status

**Beta, tylko Windows.** Kod się buduje, a testy jednostkowe przechodzą, ale program nie był jeszcze
sprawdzony na dwóch komputerach w LAN, nie ma pomiaru opóźnienia ani testu w grze. Błędy zgłaszaj w
[Issues](https://github.com/skullboypl/intmouse/issues).

## Co potrafi

- przejście kursora za krawędź ekranu, układ monitorów, zapętlanie monitorów, skok skrótem
- blokady przejścia: pełny ekran, okno zmaksymalizowane, narożniki, wciśnięty klawisz, lista programów, podwójne dotknięcie krawędzi
- schowek tekstowy, synchronizacja Caps/Num/Scroll Lock
- wysyłanie plików (chunki, potwierdzenie, anulowanie, bez katalogów)
- hasło (SHA-256), lista dozwolonych adresów IP, wybór karty sieciowej, IPv4 i IPv6
- start razem z Windows (schowany w trayu) i uruchamianie jako administrator (zakładka „Bezpieczeństwo”)
- klawisz awaryjny (przytrzymaj `Esc`), przyciemnianie nieaktywnych ekranów, ikona w trayu ze stanem (zielony bąbelek: włączone, czerwony: wyłączone)

Czego jeszcze nie ma: Linux, macOS, wysyłanie katalogów, przeciąganie plików przez krawędź ekranu,
schowek z obrazami. Pomysły i błędy zbieramy w [Issues](https://github.com/skullboypl/intmouse/issues).

## Jak używać

1. Uruchom `inputshare.exe` na obu komputerach (ta sama wersja, ten sam protokół).
2. Zezwól w zaporze na **TCP 6555** i **UDP 1046**.
3. W zakładce „Sieć" włącz „Wymagaj hasła" i wpisz **to samo hasło na obu komputerach**. Komputery z tym
   samym hasłem łączą się same, bez wpisywania adresów IP (zakładka „Połączone", opcja „Łącz automatycznie").
   Wolisz ręcznie? Dodaj adres IP drugiego komputera w „Komputery" (lista niepusta = tylko te adresy).
4. Po połączeniu monitory drugiego komputera pojawiają się w „Układzie ekranów". Układ monitorów
   jednego komputera zostaje taki jak w Windows. Przeciągasz cały drugi komputer; gdy krawędź jest
   blisko, dokleja się. Kursor przechodzi przez doklejoną krawędź. Zapisz układ.
5. Włącz udostępnianie (ikona w trayu albo `Ctrl+Shift+A`) i przesuń kursor za krawędź.

Skróty: `Ctrl+Shift+A` włącza i wyłącza udostępnianie, `Ctrl+Shift+Q` skacze na kolejny lokalny monitor,
przytrzymany `Esc` jest klawiszem awaryjnym (wyłącza udostępnianie i oddaje mysz).

**Awaryjne zamknięcie:** `Ctrl+Alt+Shift+Q` zamyka program od razu, w każdym stanie, i nie da się go wyłączyć.
Bez programu: `Ctrl+Shift+Esc` (Menedżer zadań) albo `Win+R` i `taskkill /F /IM inputshare*`.

**Powrót na swój komputer:** gdy sterujesz drugim komputerem, przesuń kursor z powrotem przez krawędź, którą
wszedł. Program liczy położenie kursora na drugim komputerze i oddaje sterowanie, nawet jeśli tamten nie odpowiada.
Przejście na drugi komputer zadziała tylko wtedy, gdy jest z nim połączenie, a komputer, który jest sterowany,
sam nie przejmuje sterowania (to powodowało „latanie” kursora na obu komputerach).

**Ruch po całym ekranie:** program bierze ruch z myszy, a nie z pozycji kursora zatrzymanej na krawędzi,
więc na drugim komputerze kursor idzie po całym ekranie. Częstotliwość (125, 500, 1000, 2000, 4000, 8000 Hz,
do limitu tej myszy) jest w Wskaźnik. Dotyczy kursora na sterowanym komputerze i nie zmienia Hz myszy w Windows.

## Budowanie

Wymagane: Rust (toolchain `stable-x86_64-pc-windows-msvc`) i Windows SDK (`rc.exe`, osadza ikonę w pliku).

```bat
cargo build -p inputshare --release   :: target\release\inputshare.exe
cargo test --workspace
build.cmd                              :: to samo, kopiuje plik do dist\
dev.cmd                                :: uruchamia UI z konsolą
```

Uwaga: program przechwytuje mysz i klawiaturę. Nie uruchamiaj go w skryptach ani na maszynie, na
której nikt nie siedzi. Testy jednostkowe nie dotykają myszy ani klawiatury.

## Struktura

| Katalog | Zawartość |
|---|---|
| `crates/protocol` | format pakietów (UDP: wejście, TCP: handshake, schowek, pliki) |
| `crates/core` | konfiguracja, układ monitorów, logika przełączania |
| `crates/app` | program: sieć, przechwytywanie i emulacja wejścia, GUI (egui), tray |
| `site/` | strona <https://intmouse.vxh.pl> (Next.js, statyczna, bez bazy i logowania) |

Zmiana formatu pakietów wymaga podbicia `PROTOCOL_VERSION` w `crates/protocol`.

## Start z systemem i administrator

Zakładka „Bezpieczeństwo” → „Uruchamianie”:

- **Uruchamiaj przy starcie systemu**: po zalogowaniu program startuje schowany w trayu (`--tray`).
  Zwykły użytkownik: wpis w `HKCU\...\Run`. Administrator: zadanie w Harmonogramie zadań z najwyższymi
  uprawnieniami (wpis w rejestrze nie uruchomi programu z uprawnieniami bez pytania przy każdym logowaniu).
- **Uruchamiaj jako administrator**: pozwala sterować oknami programów uruchomionych jako administrator
  (Windows blokuje wysyłanie wejścia do okna o wyższych uprawnieniach). Po zmianie ustawienia program zapisuje
  konfigurację i **uruchamia się ponownie** w nowym trybie (przy włączaniu Windows pyta o zgodę, UAC; po
  odmowie program działa dalej bez zmian). Przy włączonym ustawieniu zwykłe uruchomienie samo przekazuje
  sterowanie kopii z uprawnieniami. Przeciąganie plików z Eksploratora na okno nie działa w trybie administratora.

## Aktualizacje

Program sam sprawdza nowszą wersję kilka sekund po starcie i potem co 12 godzin (do wyłączenia w
zakładce „Aktualizacje”). Pobiera tylko plik <https://intmouse.vxh.pl/version.json>, w którym strona
lustruje najnowsze wydanie z GitHuba. Gdy jest nowsza wersja, u góry okna pojawia się pasek
**Aktualizuj / Później**. „Aktualizuj” pobiera plik, sprawdza jego SHA-256, podmienia exe i uruchamia
program ponownie. Poprzednia wersja zostaje jako `inputshare.exe.old` do przywrócenia.

Wydanie robisz tak: nadaj tag z numerem wersji z `Cargo.toml` (np. `v0.2.0-beta.1`), utwórz
na GitHubie wydanie i dodaj do niego podpisany `inputshare.exe` oraz archiwum `.zip`. Strona odświeża
`version.json` najwyżej po godzinie. Nowa wersja musi mieć wyższy numer niż poprzednia, bo program
porównuje numery (`0.2.0-beta.2` jest nowsze od `0.2.0-beta.1`, a `0.2.0` od obu).

## Gdy komputery się nie łączą

Na górnym pasku jest stały wskaźnik: „● połączono: …” albo „○ brak połączenia”. Zakładka **Połączone**
pokazuje, kto jest połączony, kogo widać w sieci (z przyciskiem „Połącz teraz”), opcje łączenia
automatycznego i **dziennik połączeń**. Gdy połączenie się nie udaje, na czerwono jest powód: złe hasło
albo „Wymagaj hasła” ustawione tylko na jednym komputerze, inna wersja protokołu, brak adresu na liście
drugiego komputera, zapora.

Zakładka **Logi** pokazuje pełne logi programu (ostatnie 3000 linii) z filtrem, szukaniem i kopiowaniem.
„Tryb szczegółowy” dodaje linie debug i zapisuje `inputshare.log` w folderze konfiguracji.

Łączenie automatyczne działa tylko z włączonym „Wymagaj hasła” i pustą listą w „Komputery”. Program
odmawia też połączenia z komputerem, który nie wymaga hasła, bo takiemu nasze hasło niczego nie dowodzi.
Hasło przechodzi po sieci jako skrót SHA-256 bez szyfrowania, więc używaj programu w sieci, której ufasz.

## Współpraca

Pull requesty i zgłoszenia są mile widziane. Przed PR uruchom `cargo test --workspace` i
`cargo check --workspace --all-targets` (bez ostrzeżeń). W `site/`: `npm run lint`, `npm run typecheck`, `npm test`.

## Licencja

[GNU GPL 2.0 lub nowsza](LICENSE) © 2026 Artur Spychalski. Program jest wolnym oprogramowaniem: wolno
go używać, zmieniać i rozdawać dalej na warunkach GPL (zmienione wersje też muszą mieć otwarty kod).
# INTmouse
