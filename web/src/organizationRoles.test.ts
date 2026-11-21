import { describe, expect, it } from "vitest";

import {
  ORGANIZATION_HEADING_KEYS,
  ORGANIZATION_ROLES,
  defaultRoleForType,
  organizationsHeadingKey,
} from "./organizationRoles";

describe("ORGANIZATION_ROLES", () => {
  it("defines the roles in display order", () => {
    expect(ORGANIZATION_ROLES).toEqual([
      "issued_by",
      "received_from",
      "party",
      "employer",
      "unspecified",
    ]);
  });
});

describe("defaultRoleForType", () => {
  it("returns issued_by for identity, civil, educational, financial and commercial documents", () => {
    const issuedByTypes = [
      "passport",
      "travel_passport",
      "id_card",
      "residence_permit",
      "driver_license",
      "birth_certificate",
      "marriage_certificate",
      "diploma",
      "certificate_other",
      "power_of_attorney",
      "medical_document",
      "tax_document",
      "invoice",
      "receipt",
      "payment_confirmation",
      "bank_statement",
      "warranty",
    ];

    for (const key of issuedByTypes) {
      expect(defaultRoleForType(key)).toBe("issued_by");
    }
  });

  it("returns party for contracts, rental agreements and insurance policies", () => {
    expect(defaultRoleForType("contract")).toBe("party");
    expect(defaultRoleForType("rental_agreement")).toBe("party");
    expect(defaultRoleForType("insurance_policy")).toBe("party");
  });

  it("returns received_from for letters", () => {
    expect(defaultRoleForType("letter")).toBe("received_from");
  });

  it("returns unspecified for null, undefined and custom or unknown types", () => {
    expect(defaultRoleForType(null)).toBe("unspecified");
    expect(defaultRoleForType(undefined)).toBe("unspecified");
    expect(defaultRoleForType("unknown_custom_type")).toBe("unspecified");
    expect(defaultRoleForType("article")).toBe("unspecified");
  });
});

describe("organizationsHeadingKey", () => {
  it("uses the role heading when all organizations share one non-unspecified role", () => {
    expect(organizationsHeadingKey([{ role: "issued_by" }], "letter")).toBe(
      ORGANIZATION_HEADING_KEYS.issued_by,
    );
    expect(
      organizationsHeadingKey(
        [{ role: "party" }, { role: "party" }],
        "passport",
      ),
    ).toBe(ORGANIZATION_HEADING_KEYS.party);
    expect(
      organizationsHeadingKey([{ role: "employer" }], "passport"),
    ).toBe(ORGANIZATION_HEADING_KEYS.employer);
    expect(
      organizationsHeadingKey([{ role: "received_from" }], null),
    ).toBe(ORGANIZATION_HEADING_KEYS.received_from);
  });

  it("falls back to the document type default role when organizations have mixed roles", () => {
    expect(
      organizationsHeadingKey(
        [{ role: "issued_by" }, { role: "party" }],
        "letter",
      ),
    ).toBe(ORGANIZATION_HEADING_KEYS.received_from);

    expect(
      organizationsHeadingKey(
        [{ role: "party" }, { role: "employer" }],
        "passport",
      ),
    ).toBe(ORGANIZATION_HEADING_KEYS.issued_by);
  });

  it("falls back to the document type default role when shared role is unspecified", () => {
    expect(
      organizationsHeadingKey([{ role: "unspecified" }], "contract"),
    ).toBe(ORGANIZATION_HEADING_KEYS.party);

    expect(
      organizationsHeadingKey(
        [{ role: "unspecified" }, { role: "unspecified" }],
        "passport",
      ),
    ).toBe(ORGANIZATION_HEADING_KEYS.issued_by);
  });

  it("uses the document type default role when the organization list is empty", () => {
    expect(organizationsHeadingKey([], "passport")).toBe(
      ORGANIZATION_HEADING_KEYS.issued_by,
    );
    expect(organizationsHeadingKey([], "contract")).toBe(
      ORGANIZATION_HEADING_KEYS.party,
    );
    expect(organizationsHeadingKey([], "letter")).toBe(
      ORGANIZATION_HEADING_KEYS.received_from,
    );
  });

  it("uses the generic heading when neither organizations nor the type supply a role", () => {
    expect(organizationsHeadingKey([], null)).toBe(
      ORGANIZATION_HEADING_KEYS.generic,
    );
    expect(organizationsHeadingKey([], "custom_type")).toBe(
      ORGANIZATION_HEADING_KEYS.generic,
    );
    expect(
      organizationsHeadingKey(
        [{ role: "issued_by" }, { role: "party" }],
        "custom_type",
      ),
    ).toBe(ORGANIZATION_HEADING_KEYS.generic);
    expect(
      organizationsHeadingKey(
        [{ role: "unspecified" }],
        null,
      ),
    ).toBe(ORGANIZATION_HEADING_KEYS.generic);
  });
});
