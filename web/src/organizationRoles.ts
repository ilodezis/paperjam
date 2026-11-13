import type { OrganizationRole } from "./api";

export const ORGANIZATION_ROLES: readonly OrganizationRole[] = [
  "issued_by",
  "received_from",
  "party",
  "employer",
  "unspecified",
] as const;

const ISSUED_BY_TYPES = new Set([
  "passport",
  "travel_passport",
  "id_card",
  "residence_permit",
  "driver_license",
  "birth_certificate",
  "marriage_certificate",
  "diploma",
  "certificate_other",
  "medical_document",
  "tax_document",
  "invoice",
  "receipt",
  "payment_confirmation",
  "bank_statement",
  "warranty",
]);

const PARTY_TYPES = new Set([
  "contract",
  "rental_agreement",
  "insurance_policy",
]);

const RECEIVED_FROM_TYPES = new Set([
  "letter",
]);

export function defaultRoleForType(typeKey: string | null | undefined): OrganizationRole {
  if (!typeKey) return "unspecified";
  if (ISSUED_BY_TYPES.has(typeKey)) return "issued_by";
  if (PARTY_TYPES.has(typeKey)) return "party";
  if (RECEIVED_FROM_TYPES.has(typeKey)) return "received_from";
  return "unspecified";
}

export const ORGANIZATION_HEADING_KEYS = {
  issued_by: "document.organizationHeadings.issued_by",
  received_from: "document.organizationHeadings.received_from",
  party: "document.organizationHeadings.party",
  employer: "document.organizationHeadings.employer",
  generic: "document.organizationHeadings.generic",
} as const;

export type OrganizationHeadingKey = (typeof ORGANIZATION_HEADING_KEYS)[keyof typeof ORGANIZATION_HEADING_KEYS];

export function organizationsHeadingKey(
  orgs: readonly { role?: OrganizationRole }[] | null | undefined,
  typeKey: string | null | undefined,
): OrganizationHeadingKey {
  if (orgs && orgs.length > 0) {
    const firstRole = orgs[0]?.role;
    if (firstRole && firstRole !== "unspecified" && orgs.every((org) => org.role === firstRole)) {
      if (firstRole in ORGANIZATION_HEADING_KEYS) {
        return ORGANIZATION_HEADING_KEYS[firstRole as keyof typeof ORGANIZATION_HEADING_KEYS];
      }
    }
  }

  const defaultRole = defaultRoleForType(typeKey);
  if (defaultRole !== "unspecified" && defaultRole in ORGANIZATION_HEADING_KEYS) {
    return ORGANIZATION_HEADING_KEYS[defaultRole as keyof typeof ORGANIZATION_HEADING_KEYS];
  }

  return ORGANIZATION_HEADING_KEYS.generic;
}
