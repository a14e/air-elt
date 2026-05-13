IF OBJECT_ID(N'air_elt_resume_tokens', N'U') IS NULL
BEGIN
    CREATE TABLE air_elt_resume_tokens (
        flow       NVARCHAR(255) NOT NULL PRIMARY KEY,
        token      NVARCHAR(MAX) NOT NULL,
        updated_at DATETIME2 NOT NULL DEFAULT GETUTCDATE()
    );
END;
