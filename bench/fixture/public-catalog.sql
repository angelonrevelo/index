--
-- PostgreSQL database dump
-- DummyJSON products?limit=20 frozen 2026-09-20
--

SET statement_timeout = 0;
SET lock_timeout = 0;
SET idle_in_transaction_session_timeout = 0;
SET client_encoding = 'UTF8';
SET standard_conforming_strings = on;
SET search_path = public;

CREATE TABLE public.product (sku text, name text, brand text, price numeric);

--
-- Data for Name: product; Type: TABLE DATA; Schema: public
--

INSERT INTO public.product (sku, name, brand, price) VALUES
  ('BEA-ESS-ESS-001', 'Essence Mascara Lash Princess', 'Essence', 9.99),
  ('BEA-GLA-EYE-002', 'Eyeshadow Palette with Mirror', 'Glamour Beauty', 19.99),
  ('BEA-VEL-POW-003', 'Powder Canister', 'Velvet Touch', 14.99),
  ('BEA-CHI-LIP-004', 'Red Lipstick', 'Chic Cosmetics', 12.99),
  ('BEA-NAI-NAI-005', 'Red Nail Polish', 'Nail Couture', 8.99),
  ('FRA-CAL-CAL-006', 'Calvin Klein CK One', 'Calvin Klein', 49.99),
  ('FRA-CHA-CHA-007', 'Chanel Coco Noir Eau De', 'Chanel', 129.99),
  ('FRA-DIO-DIO-008', 'Dior J''adore', 'Dior', 89.99),
  ('FRA-DOL-DOL-009', 'Dolce Shine Eau de', 'Dolce & Gabbana', 69.99),
  ('FRA-GUC-GUC-010', 'Gucci Bloom Eau de', 'Gucci', 79.99);

COPY public.product (sku, name, brand, price) FROM stdin;
FUR-ANN-ANN-011	Annibale Colombo Bed	Annibale Colombo	1899.99
FUR-ANN-ANN-012	Annibale Colombo Sofa	Annibale Colombo	2499.99
FUR-FUR-BED-013	Bedside Table African Cherry	Furniture Co.	299.99
FUR-KNO-KNO-014	Knoll Saarinen Executive Conference Chair	Knoll	499.99
FUR-BAT-WOO-015	Wooden Bathroom Sink With Mirror	Bath Trends	799.99
GRO-BRD-APP-016	Apple		1.99
GRO-BRD-BEE-017	Beef Steak		12.99
GRO-BRD-FOO-018	Cat Food		8.99
GRO-BRD-CHI-019	Chicken Meat		9.99
GRO-BRD-COO-020	Cooking Oil		4.99
\.

--
-- PostgreSQL database dump complete
--
