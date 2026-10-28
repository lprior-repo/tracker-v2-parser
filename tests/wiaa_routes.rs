use track_scrapper_v2::sources::wiaa;

#[test]
fn given_wiaa_routes_when_static_http_is_selected_then_only_exact_published_directory_routes_qualify()
 {
    for suffix in [
        "List",
        "DirectoryLetter?LetterBtn=A",
        "DirectoryLetter?LetterBtn=Z",
        "GetDirectorySchool?orgID=42",
    ] {
        assert!(wiaa::is_public_directory_url(&format!(
            "https://schools.wiaawi.org/Directory/School/{suffix}"
        )));
    }
    for url in [
        "http://schools.wiaawi.org/Directory/School/List",
        "https://schools.wiaawi.org:443/Directory/School/List",
        "https://user@schools.wiaawi.org/Directory/School/List",
        "https://schools.wiaawi.org/Directory/School/List#fragment",
        "https://schools.wiaawi.org/Directory/School/List?query=1",
        "https://schools.wiaawi.org/Directory/School/DirectoryLetter?LetterBtn=AA",
        "https://schools.wiaawi.org/Directory/School/DirectoryLetter?LetterBtn=a",
        "https://schools.wiaawi.org/Directory/School/DirectoryLetter?LetterBtn=A&other=1",
        "https://schools.wiaawi.org/Directory/School/DirectoryLetter?LetterBtn=%41",
        "https://schools.wiaawi.org/Directory/School/GetDirectorySchool?orgID=%34%32",
        "https://schools.wiaawi.org/Directory/School/SearchOrg?query=Example",
        "https://schools.wiaawi.org/Directory/School/GetDirectorySchool?orgID=0",
        "https://schools.wiaawi.org.evil.example/Directory/School/List",
        "https://other.example/Directory/School/List",
    ] {
        assert!(!wiaa::is_public_directory_url(url), "{url}");
    }
}
