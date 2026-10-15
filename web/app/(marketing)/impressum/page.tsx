import type { Metadata } from "next";
import { Fill, LegalPage } from "../../../components/legal-page";

export const metadata: Metadata = {
  title: "Impressum",
  description: "Who provides Kurbis, and how to reach them.",
};

export default function ImpressumPage() {
  return (
    <LegalPage title="Impressum" lead="Legal notice under § 5 DDG (German Digital Services Act)." updated="14 October 2026">
      <h2>Provider</h2>
      <address>
        <Fill>FULL NAME</Fill>
        <br />
        <Fill>STREET AND HOUSE NUMBER</Fill>
        <br />
        <Fill>POSTCODE AND CITY</Fill>
        <br />
        <Fill>COUNTRY</Fill>
      </address>

      <h2>Contact</h2>
      <p>
        Email: <Fill>EMAIL ADDRESS</Fill>
        <br />
        Phone: <Fill>PHONE NUMBER, OR A CONTACT FORM THAT ANSWERS QUICKLY</Fill>
      </p>

      <h2>VAT ID</h2>
      <p>
        <Fill>VAT ID UNDER § 27a UStG, OR DELETE THIS SECTION IF YOU HAVE NONE</Fill>
      </p>

      <h2>Responsible for the content</h2>
      <p>
        Under § 18 (2) MStV: <Fill>FULL NAME</Fill>, at the address above.
      </p>

      <h2>Consumer dispute resolution</h2>
      <p>We are neither willing nor obliged to take part in dispute resolution proceedings before a consumer arbitration board.</p>

      <p>
        How we handle your data is in the <a href="/privacy">privacy policy</a>.
      </p>
    </LegalPage>
  );
}
