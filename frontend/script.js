
// containers
let inputContainer;
let submitContainer;
let resultContainer;
let filesContainer;

// first load the input container and hide the submit container
window.onload = function() {
    inputContainer = document.getElementById('input');
    submitContainer = document.getElementById('submit');
    resultContainer = document.getElementById('result');
    filesContainer = document.getElementById('files');

    showInput();
}

let files;

function showInput() {
    inputContainer.style.display = 'block';
    submitContainer.style.display = 'none';
    resultContainer.style.display = 'none';
    filesContainer.style.display = 'none';
}

function chooseFile() {
    let input = document.createElement('input');

    input.type = 'file';
    input.multiple = true;

    input.onchange = e => {
        // i think this prevents some weird thing
        e.preventDefault();

        files = Array.from(input.files);

        inputContainer.style.display = 'none';
        submitContainer.style.display = 'block';

        let counter = 0;

        let displayName = files
            .filter(_ => (++counter <= 2))
            .map(f => f.name).join(', ');

        if (counter > 2) {
            displayName += ' and ' + (counter - 2) + ' more';
        }

        let fileNames = document.getElementById('file-names');
        fileNames.innerHTML = displayName;
    };
    input.click();
}

async function uploadFile() {
    let formData = new FormData();

    files.forEach((file) => {
        formData.append("upload", file);
    });

    axios({
        method: "post",
        url: "http://localhost:8080/api/v1/upload",
        data: formData,
        headers: { "Content-Type": "multipart/form-data" }
    })
        .catch((e) => console.error(e))
        .then(response => {
            showResult(response.data);
        });
}

async function listFiles() {
    inputContainer.style.display = 'none';
    resultContainer.style.display = 'none';
    filesContainer.style.display = 'block';

    let descriptionText = document.getElementById('files-description');
    descriptionText.innerHTML = "Here you can see all your files";

    // TODO make this properly when backend is done
    return await axios({
        method: "get",
        url: "http://localhost:8080/api/v1/list",
        data: "abc 1 2 3",
        headers: {"Content-Type": "text/plain"}
    })
        .catch((e) => console.error(e));
}

// TODO browse files
async function getFile() {
    return await axios({
        method: "get",
        url: "http://localhost:8080/api/v1/get",
        data: "abc 1 2 3",
        headers: {"Content-Type": "text/plain"}
    })
        .catch((e) => console.error(e));
}

// TODO download files, this can only be done after getFile is done
async function downloadFile() {
    return await axios({
        method: "get",
        url: "http://localhost:8080/api/v1/download",
        data: "abc 1 2 3",
        headers: {"Content-Type": "text/plain"}
    })
        .catch((e) => console.error(e));
}

async function showResult(response) {
    let status = response["status"];

    submitContainer.style.display = 'none';
    resultContainer.style.display = 'block';

    let button = document.getElementsByClassName('list-button')[1];

    // set button disabled
    button.style.backgroundColor = '#808080';
    button.disabled = true;
    button.style.cursor = 'not-allowed';

    // revert its color after 3 seconds
    setTimeout(() => {
        button.style.backgroundColor = '#1e90ff';
        button.disabled = false;
        button.style.cursor = 'pointer';
    }, 3000);

    if (status === true) {
        let token = response["token"];
        let input = "<input id='token-field' maxLength='8' value='" + token + "' readonly />";
        let icon = "<svg data-id=\"11\" fill=\"none\" height=\"24\" stroke=\"white\" stroke-linecap=\"round\" stroke-linejoin=\"round\" stroke-width=\"2\" viewBox=\"0 0 24 24\" width=\"24\" xmlns=\"http://www.w3.org/2000/svg\"><polyline points=\"20 6 9 17 4 12\" /></svg>";

        setResult("Successful", "Save your token you will need it", input);
        setIcon(icon);

        let tokenField = document.getElementById('token-field');

        tokenField.onclick = function() {
            navigator.clipboard.writeText(tokenField.value);
            // TODO give some feedback (popup maybe and click sound)
        }
    }
    else {
        let cause = response["error"];
        let icon = "<svg data-id=\"19\" fill=\"none\" height=\"24\" stroke=\"white\" stroke-linecap=\"round\" stroke-linejoin=\"round\" stroke-width=\"2\" viewBox=\"0 0 24 24\" width=\"24\" xmlns=\"http://www.w3.org/2000/svg\"><path d=\"m21.73 18-8-14a2 2 0 0 0-3.48 0l-8 14A2 2 0 0 0 4 21h16a2 2 0 0 0 1.73-3Z\" /><path d=\"M12 9v4\" /><path d=\"M12 17h.01\" /></svg>";

        setResult("Failed", "", cause);
        setIcon(icon);
    }
}

function setResult(title, description, alt) {
    let titleText = document.getElementById('result-title');
    let descriptionText = document.getElementById('result-description');
    let altText = document.getElementById('result-description-alt');

    descriptionText.innerHTML = description;
    titleText.innerHTML = title;
    altText.innerHTML = alt;
}

function setIcon(icon) {
    document.getElementById('status-icon').innerHTML = icon;
}