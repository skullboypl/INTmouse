import type { Metadata, Viewport } from "next";
import type { ReactNode } from "react";
import { REPO_URL, SITE_URL } from "@/lib/site";
import "./globals.css";

const TITLE = "INTmouse: mouse and keyboard sharing for Windows";
const DESCRIPTION =
  "Free open-source mouse and keyboard sharing for Windows on a LAN. Move the cursor past the screen edge onto the next PC. No account. ShareMouse alternative.";
const DESCRIPTION_PL =
  "Darmowe udostępnianie myszy i klawiatury między komputerami z Windows w sieci lokalnej. Kursor przechodzi za krawędź ekranu. Open source (GPL), bez konta.";

const jsonLd = {
  "@context": "https://schema.org",
  "@type": "SoftwareApplication",
  name: "INTmouse",
  applicationCategory: "UtilitiesApplication",
  operatingSystem: "Windows",
  url: SITE_URL,
  description: DESCRIPTION,
  abstract: DESCRIPTION_PL,
  inLanguage: ["en", "pl"],
  isAccessibleForFree: true,
  license: "https://www.gnu.org/licenses/gpl-2.0.html",
  sameAs: [REPO_URL],
  offers: { "@type": "Offer", price: "0", priceCurrency: "USD" },
};

export const metadata: Metadata = {
  metadataBase: new URL(SITE_URL),
  applicationName: "INTmouse",
  title: TITLE,
  description: DESCRIPTION,
  keywords: [
    "INTmouse",
    "mouse and keyboard sharing",
    "share mouse between computers",
    "share keyboard between PCs",
    "ShareMouse alternative",
    "open source",
    "Windows",
    "LAN",
    "udostępnianie myszy i klawiatury",
    "jedna mysz kilka komputerów",
  ],
  alternates: { canonical: "/" },
  openGraph: {
    type: "website",
    url: SITE_URL,
    siteName: "INTmouse",
    title: TITLE,
    description: DESCRIPTION,
    locale: "en_US",
    alternateLocale: ["pl_PL"],
  },
  twitter: { card: "summary", title: TITLE, description: DESCRIPTION },
};

export const viewport: Viewport = {
  themeColor: "#090a0c",
};

export default function RootLayout({
  children,
}: Readonly<{
  children: ReactNode;
}>) {
  return (
    <html lang="pl">
      <body className="antialiased">
        <script type="application/ld+json" dangerouslySetInnerHTML={{ __html: JSON.stringify(jsonLd) }} />
        {children}
      </body>
    </html>
  );
}
