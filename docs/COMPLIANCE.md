# Nonprofit compliance

What keeps Sun Brewed Homestead Inc in good standing, when it's due, and
where each rule comes from. Tracked as issues in the Linear project
**Nonprofit compliance**. Researched 2026-09-28. Rules change, so check the
linked source before filing, and have an accountant or attorney confirm
anything marked *confirm*.

## Who we are, per the IRS

From the IRS Exempt Organizations Business Master File (via
[ProPublica](https://projects.propublica.org/nonprofits/organizations/920510478)),
checked 2026-09-28:

| Field | Value |
|---|---|
| Legal name | Sun Brewed Homestead Inc |
| EIN | 92-0510478 |
| Status | 501(c)(3) public charity; contributions deductible |
| IRS ruling date | 2026-07-01 |
| Fiscal year ends | June 30 (accounting period 06) |
| Annual return | 990-N (gross receipts normally $50,000 or less) |
| Mailing address on file | 11333 Moorpark St PMB 216, Studio City, CA 91602 | <!-- oss-publish:allow: public org mailbox / county line -->
| NTEE | C42 (garden club / horticultural programs) |

The website's Donate page still says "we are not yet tax exempt". That is
out of date; see the Website refresh project in Linear.

## Calendar

Because the fiscal year ends June 30, most annual filings fall on the same
day: **November 15**.

| Due | Filing | Who with | Source |
|---|---|---|---|
| Nov 15 every year | **Form 990-N** (e-Postcard) | IRS | "due every year by the 15th day of the 5th month after the close of your tax year" ([IRS](https://www.irs.gov/charities-non-profits/annual-electronic-filing-requirement-for-small-exempt-organizations-form-990-n-e-postcard)). Three missed years in a row revokes exemption automatically. |
| Nov 15 every year | **FTB 199N** (California e-Postcard) | Franchise Tax Board | Same 15th-day-of-5th-month rule for gross receipts normally $50,000 or less ([FTB 199 booklet](https://www.ftb.ca.gov/forms/2025/2025-199-booklet.html)). |
| Nov 15 every year | **RRF-1** (+ **CT-TR-1** when filing 990-N), $25 fee under $50,000 revenue | CA Attorney General, Registry of Charitable Trusts | "due 4 months and 15 days after the charitable organization's fiscal year ends" ([AG renewals](https://oag.ca.gov/charities/renewals)) |
| Within 30 days of first receiving charitable assets | **CT-1** initial registration, $50 | Registry of Charitable Trusts | [AG initial registration](https://oag.ca.gov/charities/initial-reg). Donations, adoption fees and in-kind gifts all count, so if we haven't registered, we're late. *Confirm status first.* |
| Once | **FTB 3500A** with the IRS determination letter | Franchise Tax Board | Gives California exemption from franchise tax, based on the federal 501(c)(3). *Confirm it's been filed.* |
| Every 2 years, by the end of the incorporation month | **SI-100** Statement of Information, $20 ($250 late penalty) | CA Secretary of State | Incorporation month is the "Date Filed" in [bizfile](https://bizfileonline.sos.ca.gov/search/business) ([CalNonprofits checklist](https://calnonprofits.org/resources/nonprofit-compliance-checklist/)) |

## Registrations the farm stand and animals need

- **Egg handler registration (CDFA).** "Anyone engaged in the production,
  sale, or handling of shell eggs … must register … There are no
  exemptions to the registration requirement." Eggs must be kept at 45 °F or
  less. ([CDFA Egg Safety and Quality Management](https://www.cdfa.ca.gov/AHFSS/mpes/esqm.html))
  We sell eggs at $7 a dozen, so this applies now.
- **Egg cartons, grading and price signs (CDFA).** Per CDFA's
  [quick reference](https://www.cdfa.ca.gov/ahfss/mpes/pdfs/Quick_Reference_Guide.pdf)
  (read 2026-09-29): each carton carries the packer's name, address and zip,
  the Julian packing date, a sell-by date, "Keep Refrigerated" or the safe
  handling statement, and the unabbreviated size and grade plus "Shell Egg
  Food Safety Compliant", those three at least ¼ inch tall. Selling
  ungraded eggs is unlawful, so eggs are candled and sized first. Any sign
  or post giving the price of eggs must also give the full size and grade.
  The app prints these labels at **Farm stand → Egg carton labels**, and the
  stand sign states size and grade next to the egg price. The one label
  exemption (sold on the premises to household consumers, flock of 500 or
  fewer) applies only **without advertising by the producer**. We post the
  stand on Instagram and the website, so assume it doesn't apply. The
  sell-by default is 30 days (USDA's limit for eggs it grades); *confirm*
  California's.
- **Seller's permit (CDTFA).** Nonprofits generally need one to sell goods,
  even untaxed ones, and there is no blanket nonprofit exemption. Food
  produce and eggs are generally not taxed; compost bags may be. *Confirm
  with CDTFA* ([Publication 18](https://www.cdtfa.ca.gov/formspubs/pub18.pdf)).
- **City of LA business tax registration.** *Confirm* whether the Office of
  Finance needs a registration plus nonprofit exemption for the stand.

## Land use at the farm (*confirm*)

About 26 hens, possibly goats, and a farm stand on a lot in the City of Los
Angeles. The rules depend on the lot's zone, which
[ZIMAS](https://zimas.lacity.org/) shows by address. What secondary sources
say, still to confirm with LA Animal Services ((888) 452-7381) and City <!-- oss-publish:allow: public org mailbox / county line -->
Planning:

- Hens are allowed in residential zones for non-commercial keeping, at
  least 20 ft from the owner's dwelling and 35 ft from neighbours'
  (LAMC §53.59 and Animal Services rules). Whether selling eggs at a stand
  counts as commercial here is the open question.
- Goats and other livestock depend on the zone (the A and RA zones allow
  more). Check before promising "goats" to donors.
- The farm stand itself may need the zone to allow it.

## Donor paperwork

- **$250+ gifts need a written acknowledgment**, cash or in kind. It must
  state the cash amount, or *describe* (not value) donated property, and say
  whether we gave any goods or services in return
  ([IRS Pub 1771](https://www.irs.gov/pub/irs-pdf/p1771.pdf)). The core
  app's **Gifts** page tracks which gifts still need one and prints it.
  Zeffy sends receipts for its own donations; in-kind gifts, sponsors and
  cash handed over at the stand are on us.
- A payment over $75 that is partly for goods or services (a dinner ticket,
  an adoption fee with supplies) needs a disclosure of the value received.

## Not a filing, but a gap

- **General liability insurance.** Waivers don't replace it. We host the
  public, run workshops with kids, keep animals and sell food.
- **Legal releases have not been reviewed by counsel.** The steward
  approved them for use on 2026-09-29 without a lawyer (`approved` and
  `reviewed_by_counsel` in `documenso/releases.toml`); a California
  attorney should still read them when the budget allows.
