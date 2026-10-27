ALTER TABLE schools DROP CONSTRAINT schools_source_check;
ALTER TABLE schools ADD CONSTRAINT schools_source_check CHECK (source IN ('athleticnet', 'milesplit', 'nces'));
